//! Embedded Web admin panel.
//!
//! The panel is deliberately separate from the `/v1` inference API:
//!
//! * admin routes authenticate with an Argon2id password, an HttpOnly session
//!   cookie and a CSRF header; a gateway API key never grants admin access;
//! * account and credential writes always go through `AccountService` /
//!   `CredentialService`;
//! * provider-specific OAuth lives in the CodeArts adapter, not in handlers;
//! * static assets are embedded in the binary (no Node.js at runtime).

mod account;
mod api;
mod auth;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use parking_lot::Mutex;
use rust_embed::RustEmbed;
use serde_json::json;

use crate::account_service::AccountService;
use crate::api::ModelCatalog;
use crate::checkin::CheckinService;
use crate::config::Config;
use crate::error::GatewayError;
use crate::providers::codearts::login::LoginConfig;
use crate::providers::profile::Identity;
use crate::providers::qoder::auth::QoderAuthClient;
use crate::providers::trae::auth::TraeAuthClient;
use crate::providers::workbuddy::auth::WorkbuddyAuthClient;
use crate::providers::ProviderRegistry;
use crate::security::SecretBox;
use crate::storage::Storage;

pub use auth::AdminPrincipal;
pub use auth::LoginLimiter;

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

/// Non-secret runtime settings derived from `[admin]` plus the listener.
#[derive(Debug, Clone)]
pub struct AdminSettings {
    pub redirect_url: String,
    pub flow_ttl: Duration,
    pub session_ttl: Duration,
    pub secure_cookie: Option<bool>,
    pub allowed_origins: Vec<String>,
    pub public_url: Option<String>,
    pub trust_proxy: bool,
    pub setup_local_only: bool,
    pub login_max_failures: usize,
    pub login_window: Duration,
    /// Request-log retention in days; `None` keeps rows until purged by hand.
    pub log_retention_days: Option<u32>,
    pub codearts: LoginConfig,
    pub officeace: crate::providers::officeace::login::LoginConfig,
}

impl AdminSettings {
    fn from_config(config: &Config, listener_port: u16) -> Self {
        let admin = &config.admin;
        let redirect_url = admin
            .redirect_url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{listener_port}/oauth/callback"));
        Self {
            redirect_url,
            flow_ttl: Duration::from_secs(admin.flow_ttl_secs),
            session_ttl: Duration::from_secs(config.auth.admin_session_ttl_hours * 3600),
            secure_cookie: admin.secure_cookie,
            allowed_origins: admin.allowed_origins.clone(),
            public_url: config.server.public_url.clone(),
            trust_proxy: config.server.trust_proxy,
            setup_local_only: admin.setup_local_only,
            login_max_failures: admin.login_max_failures as usize,
            login_window: Duration::from_secs(admin.login_window_secs),
            log_retention_days: config.metrics.log_retention_days,
            codearts: LoginConfig {
                client_id: admin.codearts_client_id.clone(),
                uri_scheme: admin.codearts_uri_scheme.clone(),
                theme: admin.codearts_theme.clone(),
                plugin_name: admin.codearts_plugin_name.clone(),
                plugin_version: admin.codearts_plugin_version.clone(),
                locale: admin.codearts_locale.clone(),
                cn_portal_url: admin.codearts_cn_portal_url.clone(),
                intl_portal_url: admin.codearts_intl_portal_url.clone(),
                sts_url: admin.codearts_sts_url.clone(),
            },
            officeace: officeace_login_config(config),
        }
    }
}

/// OfficeAce control-plane endpoints follow the provider entry so a private
/// deployment and the tests can point the flow somewhere else.
fn officeace_login_config(config: &Config) -> crate::providers::officeace::login::LoginConfig {
    use crate::providers::officeace::region;
    let provider = config.providers.iter().find(|provider| {
        provider.kind == crate::config::ProviderKind::Officeace && provider.enabled
    });
    let option = |key: &str| {
        provider
            .and_then(|provider| provider.options.get(key))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    crate::providers::officeace::login::LoginConfig {
        cloud: option("cloud_url").unwrap_or_else(|| region::CLOUD_HOST.into()),
        sts: option("sts_url").unwrap_or_else(|| region::STS_HOST.into()),
        ..Default::default()
    }
}

#[derive(Clone)]
pub struct AdminState {
    pub storage: Storage,
    pub accounts: AccountService,
    pub registry: Arc<ProviderRegistry>,
    pub catalog: Arc<ModelCatalog>,
    pub checkin: CheckinService,
    pub secret_box: Arc<SecretBox>,
    pub settings: Arc<AdminSettings>,
    pub http: reqwest::Client,
    pub qoder_auth: QoderAuthClient,
    pub qoder_flow_locks: FlowLocks,
    pub officeace_flow_locks: FlowLocks,
    pub workbuddy_auth: WorkbuddyAuthClient,
    pub workbuddy_flow_locks: FlowLocks,
    pub workbuddy_poll_guard: PollThrottle,
    pub trae_auth: TraeAuthClient,
    pub trae_flow_locks: FlowLocks,
    pub limiter: LoginLimiter,
    /// In-process scheduler health; the runtime replaces this with its shared
    /// instance so the dashboard reflects real rotation state.
    pub scheduler: Arc<crate::scheduler::Scheduler>,
}

/// Serializes upstream polling per flow. Device-token polling is read-only, but
/// two concurrent status requests could both fetch a token and race to persist
/// the account; one poll at a time keeps the flow state single-owner.
#[derive(Clone, Default)]
pub struct FlowLocks {
    inner: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl FlowLocks {
    pub async fn lock(&self, flow_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.inner.lock();
            Arc::clone(
                map.entry(flow_id.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }
}

/// Keeps the UI from polling a device-code endpoint faster than the upstream
/// asks. One entry per flow; a denied poll returns the current pending view.
#[derive(Clone)]
pub struct PollThrottle {
    inner: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    interval: Duration,
}

impl Default for PollThrottle {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            // The WorkBuddy device-code endpoint asks for a 5 s poll interval.
            interval: Duration::from_secs(5),
        }
    }
}

impl PollThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            interval,
        }
    }

    pub fn allow(&self, flow_id: &str) -> bool {
        if self.interval.is_zero() {
            return true;
        }
        let now = std::time::Instant::now();
        let mut map = self.inner.lock();
        match map.get(flow_id) {
            Some(last) if now.duration_since(*last) < self.interval => false,
            _ => {
                map.insert(flow_id.to_string(), now);
                true
            }
        }
    }
}

impl AdminState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &Config,
        listener_port: u16,
        storage: Storage,
        accounts: AccountService,
        registry: Arc<ProviderRegistry>,
        catalog: Arc<ModelCatalog>,
        checkin: CheckinService,
        secret_box: Arc<SecretBox>,
    ) -> Self {
        let settings = Arc::new(AdminSettings::from_config(config, listener_port));
        let limiter = LoginLimiter::new(
            settings.login_max_failures,
            settings.login_window,
            Arc::new(Mutex::new(std::collections::HashMap::new())),
        );
        let http = reqwest::Client::new();
        let qoder_auth = QoderAuthClient::new(http.clone());
        // The Intl portal rejects requests that lack the official client's
        // header family, so the admin authorization client follows the
        // provider option instead of always sending honest headers.
        let workbuddy_identity = config
            .providers
            .iter()
            .filter(|provider| matches!(provider.kind, crate::config::ProviderKind::Workbuddy))
            .filter_map(|provider| provider.options.get("client.identity"))
            .find_map(|raw| raw.trim().parse::<Identity>().ok())
            .unwrap_or_default();
        let workbuddy_auth =
            WorkbuddyAuthClient::new(http.clone()).with_identity(workbuddy_identity);
        let trae_auth = TraeAuthClient::new(http.clone());
        Self {
            storage,
            accounts,
            registry,
            catalog,
            checkin,
            secret_box,
            settings,
            http,
            qoder_auth,
            qoder_flow_locks: FlowLocks::default(),
            officeace_flow_locks: FlowLocks::default(),
            workbuddy_auth,
            workbuddy_flow_locks: FlowLocks::default(),
            workbuddy_poll_guard: PollThrottle::default(),
            trae_auth,
            trae_flow_locks: FlowLocks::default(),
            limiter,
            scheduler: Arc::new(crate::scheduler::Scheduler::default()),
        }
    }
}

/// Build the admin router. Merged with the `/v1` router by the runtime.
pub fn router(state: AdminState) -> Router {
    let public = Router::new()
        .route("/admin/api/bootstrap", get(auth::bootstrap))
        .route("/admin/api/setup", post(auth::setup))
        .route("/admin/api/login", post(auth::login))
        .route("/admin/api/recover", post(auth::recover))
        .route("/oauth/callback", get(api::oauth_callback));
    let protected = Router::new()
        .route("/admin/api/session", get(auth::session_info))
        .route("/admin/api/logout", post(auth::logout))
        .route("/admin/api/dashboard", get(api::dashboard))
        .route("/admin/api/providers", get(api::providers))
        .route("/admin/api/models", get(api::models))
        .route(
            "/admin/api/model-routes",
            get(api::model_routes).post(api::create_model_route),
        )
        .route(
            "/admin/api/model-routes/{id}",
            axum::routing::patch(api::patch_model_route).delete(api::delete_model_route),
        )
        .route("/admin/api/scheduler", get(api::scheduler))
        .route("/admin/api/logs", get(api::request_logs))
        .route("/admin/api/logs/purge", post(api::purge_request_logs))
        .route("/admin/api/checkins", get(api::checkins))
        .route("/admin/api/usage", get(api::usage))
        .route(
            "/admin/api/accounts",
            get(api::list_accounts).post(api::create_account),
        )
        .route(
            "/admin/api/accounts/{id}",
            get(api::get_account)
                .patch(api::patch_account)
                .delete(api::delete_account),
        )
        .route(
            "/admin/api/accounts/{id}/auth-status",
            get(api::local_auth_status).post(api::refresh_auth_status),
        )
        .route(
            "/admin/api/accounts/{id}/quota",
            get(api::get_quota).post(api::refresh_quota),
        )
        .route(
            "/admin/api/accounts/{id}/checkins",
            get(api::account_checkins),
        )
        .route(
            "/admin/api/accounts/{id}/checkin",
            post(api::run_account_checkin),
        )
        .route(
            "/admin/api/auth/active/cancel",
            post(api::cancel_active_auth),
        )
        .route(
            "/admin/api/auth/loomy/session",
            post(api::import_loomy_session),
        )
        .route(
            "/admin/api/auth/codearts/start",
            post(api::start_codearts_auth),
        )
        .route(
            "/admin/api/auth/codearts/{flow_id}",
            get(api::codearts_flow_status),
        )
        .route(
            "/admin/api/auth/codearts/{flow_id}/complete",
            post(api::complete_codearts_auth),
        )
        .route(
            "/admin/api/auth/codearts/{flow_id}/cancel",
            post(api::cancel_codearts_flow),
        )
        .route(
            "/admin/api/auth/officeace/start",
            post(api::start_officeace_auth),
        )
        .route(
            "/admin/api/auth/officeace/{flow_id}",
            get(api::officeace_flow_status),
        )
        .route(
            "/admin/api/auth/officeace/{flow_id}/cancel",
            post(api::cancel_officeace_flow),
        )
        .route("/admin/api/auth/qoder/start", post(api::start_qoder_auth))
        .route(
            "/admin/api/auth/qoder/{flow_id}",
            get(api::qoder_flow_status),
        )
        .route(
            "/admin/api/auth/qoder/{flow_id}/cancel",
            post(api::cancel_qoder_flow),
        )
        .route(
            "/admin/api/auth/workbuddy/start",
            post(api::start_workbuddy_auth),
        )
        .route(
            "/admin/api/auth/workbuddy/{flow_id}",
            get(api::workbuddy_flow_status),
        )
        .route(
            "/admin/api/auth/workbuddy/{flow_id}/cancel",
            post(api::cancel_workbuddy_flow),
        )
        .route("/admin/api/auth/trae/start", post(api::start_trae_auth))
        .route("/admin/api/auth/trae/{flow_id}", get(api::trae_flow_status))
        .route(
            "/admin/api/auth/trae/{flow_id}/complete",
            post(api::complete_trae_auth),
        )
        .route(
            "/admin/api/auth/trae/{flow_id}/cancel",
            post(api::cancel_trae_flow),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        ));
    let pages = Router::new()
        .route("/", get(index))
        .route("/admin", get(|| async { Redirect::permanent("/admin/") }))
        .route("/admin/", get(index))
        .route("/admin/static/{*path}", get(static_asset));

    Router::new()
        .merge(public)
        .merge(protected)
        .merge(pages)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn index() -> Response {
    match Assets::get("index.html") {
        Some(file) => {
            let mut response = Html(file.data).into_response();
            no_store(&mut response);
            response
        }
        None => (StatusCode::INTERNAL_SERVER_ERROR, "admin UI is missing").into_response(),
    }
}

async fn static_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    if path.contains("..") || path.starts_with('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(file) = Assets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    let mut response = Response::new(Body::from(file.data.into_owned()));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    no_store(&mut response);
    response
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({"error": {"code": "not_found", "message": "not found"}})),
    )
        .into_response()
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    if path.starts_with("/admin") || path.starts_with("/oauth/") {
        no_store(&mut response);
    }
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

pub(crate) fn no_store(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

pub(crate) fn json_error(error: &GatewayError) -> Response {
    let mut response = (
        error.kind.http_status(),
        axum::Json(json!({
            "error": {
                "code": error.kind.code(),
                "message": error.message,
            }
        })),
    )
        .into_response();
    no_store(&mut response);
    response
}
