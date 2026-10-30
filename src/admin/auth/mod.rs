//! Administrator authentication: first-run setup, password login, recovery,
//! HttpOnly sessions and CSRF checks.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, Method},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::error::{ErrorKind, GatewayError};
use crate::security::{generate_token, hash_token, verify_token};

mod flow;
mod officeace;
mod qoder;
mod trae;
mod workbuddy;

use super::{json_error, no_store, AdminState};
use crate::storage::AdminSessionRow;

const COOKIE: &str = "polygate_admin";

/// Peer address inserted by `into_make_service_with_connect_info`. Optional so
/// unit tests can drive the router without a real socket.
pub struct AdminPeer(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for AdminPeer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|value| value.0),
        ))
    }
}
const MIN_PASSWORD: usize = 12;
const MAX_PASSWORD: usize = 256;

#[derive(Clone, Debug)]
pub struct AdminPrincipal {
    pub session_id: String,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct LoginLimiter {
    inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    max: usize,
    window: Duration,
}

impl LoginLimiter {
    pub fn new(
        max: usize,
        window: Duration,
        inner: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    ) -> Self {
        Self { inner, max, window }
    }

    fn retry_after(&self, key: &str) -> Option<u64> {
        let mut map = self.inner.lock();
        let failures = map.entry(key.to_string()).or_default();
        while failures
            .front()
            .is_some_and(|at| at.elapsed() > self.window)
        {
            failures.pop_front();
        }
        if failures.len() >= self.max {
            let retry = failures
                .front()
                .map(|at| self.window.saturating_sub(at.elapsed()).as_secs() + 1)
                .unwrap_or(1);
            Some(retry)
        } else {
            None
        }
    }

    fn fail(&self, key: &str) {
        self.inner
            .lock()
            .entry(key.to_string())
            .or_default()
            .push_back(Instant::now());
    }

    fn clear(&self, key: &str) {
        self.inner.lock().remove(key);
    }
}

// ---------------------------------------------------------------------------
// Password helpers
// ---------------------------------------------------------------------------

fn validate_password(password: &str) -> Result<(), GatewayError> {
    if password.len() < MIN_PASSWORD || password.len() > MAX_PASSWORD {
        return Err(GatewayError::bad_request(format!(
            "administrator password must be {MIN_PASSWORD}..={MAX_PASSWORD} bytes"
        )));
    }
    let lowered = password.to_ascii_lowercase();
    if [
        "password",
        "12345678",
        "adminadmin",
        "changeme",
        "qwertyuiop",
    ]
    .iter()
    .any(|weak| lowered.contains(weak))
    {
        return Err(GatewayError::bad_request(
            "administrator password is too easy to guess",
        ));
    }
    Ok(())
}

async fn hash_password(password: String) -> Result<String, GatewayError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| GatewayError::internal("password hashing failed"))
    })
    .await
    .map_err(|_| GatewayError::internal("password hashing task failed"))?
}

async fn verify_password(hash: String, password: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .unwrap_or(false)
}

fn new_recovery_code() -> String {
    generate_token("pgrec-")
}

fn recovery_key(peer: Option<SocketAddr>) -> String {
    format!("recover:{}", peer_key(peer))
}

fn peer_key(peer: Option<SocketAddr>) -> String {
    peer.map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "local".into())
}

// ---------------------------------------------------------------------------
// Sessions and cookies
// ---------------------------------------------------------------------------

struct NewSession {
    token: String,
    csrf: String,
    expires_at: DateTime<Utc>,
}

async fn create_session(state: &AdminState) -> Result<NewSession, GatewayError> {
    let token = generate_token("pgadm-");
    let csrf = generate_token("csrf-");
    let id = Uuid::new_v4().to_string();
    let expires_at = Utc::now()
        + chrono::Duration::from_std(state.settings.session_ttl)
            .unwrap_or_else(|_| chrono::Duration::hours(24));
    let admin = state.storage.admin();
    let _ = admin.prune_sessions(Utc::now()).await;
    admin
        .create_session(&id, &hash_token(&token), &csrf, expires_at)
        .await?;
    Ok(NewSession {
        token,
        csrf,
        expires_at,
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|part| {
                let (key, value) = part.trim().split_once('=')?;
                (key == name).then(|| value.to_string())
            })
        })
}

fn is_secure(state: &AdminState, headers: &HeaderMap) -> bool {
    if let Some(value) = state.settings.secure_cookie {
        return value;
    }
    if state
        .settings
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://"))
    {
        return true;
    }
    state.settings.trust_proxy
        && headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

fn session_cookie(state: &AdminState, headers: &HeaderMap, token: &str) -> String {
    let max_age = state.settings.session_ttl.as_secs();
    let secure = if is_secure(state, headers) {
        "; Secure"
    } else {
        ""
    };
    format!("{COOKIE}={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}")
}

fn clear_cookie(state: &AdminState, headers: &HeaderMap) -> String {
    let secure = if is_secure(state, headers) {
        "; Secure"
    } else {
        ""
    };
    format!("{COOKIE}=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0{secure}")
}

fn with_cookie(mut response: Response, cookie: String) -> Response {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn unauthorized() -> Response {
    let mut response = (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(json!({"error": {"code": "admin_auth_required", "message": "administrator login required"}})),
    )
        .into_response();
    no_store(&mut response);
    response
}

async fn authenticate(state: &AdminState, headers: &HeaderMap) -> Option<AdminPrincipal> {
    let token = cookie_value(headers, COOKIE)?;
    let session = state
        .storage
        .admin()
        .session_by_hash(&hash_token(&token))
        .await
        .ok()??;
    validate_session(session)
}

fn validate_session(session: AdminSessionRow) -> Option<AdminPrincipal> {
    if session.revoked_at.is_some() || session.expires_at <= Utc::now() {
        return None;
    }
    Some(AdminPrincipal {
        session_id: session.id,
        csrf_token: session.csrf_token,
        expires_at: session.expires_at,
    })
}

/// Authentication + CSRF + Origin middleware for mutating admin routes.
pub async fn require_admin(
    State(state): State<AdminState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(principal) = authenticate(&state, request.headers()).await else {
        return unauthorized();
    };
    let method = request.method().clone();
    if is_unsafe(&method) {
        if !origin_allowed(request.headers(), &state) {
            return forbidden("request origin is not allowed");
        }
        let presented = request
            .headers()
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if presented.is_empty() || !verify_token(presented, &hash_token(&principal.csrf_token)) {
            return forbidden("CSRF token is missing or invalid");
        }
    }
    let _ = state
        .storage
        .admin()
        .touch_session(&principal.session_id)
        .await;
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn forbidden(message: &str) -> Response {
    let mut response = (
        axum::http::StatusCode::FORBIDDEN,
        Json(json!({"error": {"code": "admin_forbidden", "message": message}})),
    )
        .into_response();
    no_store(&mut response);
    response
}

fn is_unsafe(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

pub(crate) fn origin_allowed(headers: &HeaderMap, state: &AdminState) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(origin_url) = url::Url::parse(origin) else {
        return false;
    };
    if !matches!(origin_url.scheme(), "http" | "https") {
        return false;
    }
    let origin = origin_url.origin().ascii_serialization();

    if let Some(public) = &state.settings.public_url {
        if let Ok(public) = url::Url::parse(public) {
            if public.origin().ascii_serialization() == origin {
                return true;
            }
        }
    }
    if state
        .settings
        .allowed_origins
        .iter()
        .any(|allowed| allowed.trim_end_matches('/') == origin)
    {
        return true;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let scheme = if state.settings.trust_proxy
        && headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
    {
        "https"
    } else {
        "http"
    };
    url::Url::parse(&format!("{scheme}://{host}"))
        .map(|expected| expected.origin().ascii_serialization() == origin)
        .unwrap_or(false)
}

fn setup_allowed(state: &AdminState, peer: Option<SocketAddr>, headers: &HeaderMap) -> bool {
    if !state.settings.setup_local_only {
        return true;
    }
    if !peer.is_some_and(|addr| addr.ip().is_loopback()) {
        return false;
    }
    // A reverse proxy on the same host makes the TCP peer loopback while the
    // request Host is public; require a loopback Host as well.
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let host = host.rsplit_once(':').map(|(host, _)| host).unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn bootstrap(State(state): State<AdminState>) -> Response {
    let initialized = state.storage.admin().user().await.ok().flatten().is_some();
    let mut response = Json(json!({
        "initialized": initialized,
        "setup_local_only": state.settings.setup_local_only,
        "oauth_callback_url": state.settings.redirect_url,
        "version": crate::VERSION,
        "codearts_regions": [
            {"id": "cn", "label": "中国站"},
            {"id": "intl", "label": "国际站"}
        ]
    }))
    .into_response();
    no_store(&mut response);
    response
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub password: String,
}

pub async fn setup(
    State(state): State<AdminState>,
    peer: AdminPeer,
    headers: HeaderMap,
    Json(request): Json<SetupRequest>,
) -> Response {
    if !origin_allowed(&headers, &state) {
        return forbidden("request origin is not allowed");
    }
    if !setup_allowed(&state, peer.0, &headers) {
        return forbidden("administrator setup is limited to loopback");
    }
    if let Err(error) = validate_password(&request.password) {
        return json_error(&error);
    }
    let Ok(existing) = state.storage.admin().user().await else {
        return json_error(&GatewayError::internal(
            "administrator store is unavailable",
        ));
    };
    if existing.is_some() {
        return json_error(&GatewayError::new(
            ErrorKind::BadRequest,
            "administrator is already initialized",
        ));
    }
    let Ok(password_hash) = hash_password(request.password).await else {
        return json_error(&GatewayError::internal("password hashing failed"));
    };
    let recovery = new_recovery_code();
    let recovery_hash = hash_token(&recovery);
    let created = state
        .storage
        .admin()
        .create_user("admin", &password_hash, &recovery_hash)
        .await;
    match created {
        Ok(true) => {}
        Ok(false) => {
            return json_error(&GatewayError::new(
                ErrorKind::BadRequest,
                "administrator is already initialized",
            ))
        }
        Err(error) => return json_error(&error),
    }
    let session = match create_session(&state).await {
        Ok(session) => session,
        Err(error) => return json_error(&error),
    };
    let mut response = Json(json!({
        "username": "admin",
        "csrf_token": session.csrf,
        "expires_at": session.expires_at,
        "recovery_code": recovery,
    }))
    .into_response();
    no_store(&mut response);
    with_cookie(response, session_cookie(&state, &headers, &session.token))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<AdminState>,
    peer: AdminPeer,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    if !origin_allowed(&headers, &state) {
        return forbidden("request origin is not allowed");
    }
    let key = format!("login:{}", peer_key(peer.0));
    if let Some(seconds) = state.limiter.retry_after(&key) {
        return too_many(seconds);
    }
    let Ok(Some(user)) = state.storage.admin().user().await else {
        return json_error(&GatewayError::new(
            ErrorKind::BadRequest,
            "administrator is not initialized",
        ));
    };
    let username_ok =
        request.username.trim().is_empty() || request.username.trim() == user.username;
    let password_ok = verify_password(user.password_hash.clone(), request.password).await;
    if !username_ok || !password_ok {
        state.limiter.fail(&key);
        return unauthorized();
    }
    state.limiter.clear(&key);
    let session = match create_session(&state).await {
        Ok(session) => session,
        Err(error) => return json_error(&error),
    };
    let mut response = Json(json!({
        "username": user.username,
        "csrf_token": session.csrf,
        "expires_at": session.expires_at,
    }))
    .into_response();
    no_store(&mut response);
    with_cookie(response, session_cookie(&state, &headers, &session.token))
}

#[derive(Deserialize)]
pub struct RecoverRequest {
    pub recovery_code: String,
    pub new_password: String,
}

pub async fn recover(
    State(state): State<AdminState>,
    peer: AdminPeer,
    headers: HeaderMap,
    Json(request): Json<RecoverRequest>,
) -> Response {
    if !origin_allowed(&headers, &state) {
        return forbidden("request origin is not allowed");
    }
    let key = recovery_key(peer.0);
    if let Some(seconds) = state.limiter.retry_after(&key) {
        return too_many(seconds);
    }
    if let Err(error) = validate_password(&request.new_password) {
        return json_error(&error);
    }
    let Ok(Some(user)) = state.storage.admin().user().await else {
        return json_error(&GatewayError::new(
            ErrorKind::BadRequest,
            "administrator is not initialized",
        ));
    };
    if !verify_token(request.recovery_code.trim(), &user.recovery_hash) {
        state.limiter.fail(&key);
        return unauthorized();
    }
    let Ok(password_hash) = hash_password(request.new_password).await else {
        return json_error(&GatewayError::internal("password hashing failed"));
    };
    let recovery = new_recovery_code();
    let updated = state
        .storage
        .admin()
        .set_credentials(&password_hash, &hash_token(&recovery))
        .await;
    match updated {
        Ok(true) => {}
        Ok(false) => {
            return json_error(&GatewayError::new(
                ErrorKind::BadRequest,
                "administrator is not initialized",
            ))
        }
        Err(error) => return json_error(&error),
    }
    if let Err(error) = state.storage.admin().revoke_all_sessions().await {
        return json_error(&error);
    }
    let session = match create_session(&state).await {
        Ok(session) => session,
        Err(error) => return json_error(&error),
    };
    let mut response = Json(json!({
        "username": user.username,
        "csrf_token": session.csrf,
        "expires_at": session.expires_at,
        "recovery_code": recovery,
    }))
    .into_response();
    no_store(&mut response);
    with_cookie(response, session_cookie(&state, &headers, &session.token))
}

pub async fn session_info(
    State(state): State<AdminState>,
    axum::Extension(principal): axum::Extension<AdminPrincipal>,
) -> Response {
    let user = state
        .storage
        .admin()
        .user()
        .await
        .ok()
        .flatten()
        .map(|user| user.username)
        .unwrap_or_else(|| "admin".into());
    let mut response = Json(json!({
        "username": user,
        "csrf_token": principal.csrf_token,
        "expires_at": principal.expires_at,
    }))
    .into_response();
    no_store(&mut response);
    response
}

pub async fn logout(
    State(state): State<AdminState>,
    headers: HeaderMap,
    axum::Extension(principal): axum::Extension<AdminPrincipal>,
) -> Response {
    let _ = state
        .storage
        .admin()
        .revoke_session(&principal.session_id)
        .await;
    let mut response = Json(json!({"logged_out": true})).into_response();
    no_store(&mut response);
    with_cookie(response, clear_cookie(&state, &headers))
}

fn too_many(seconds: u64) -> Response {
    let mut response = (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error": {"code": "rate_limit_exceeded", "message": "too many failed attempts"}})),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    no_store(&mut response);
    response
}
