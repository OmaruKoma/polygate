//! Admin panel integration tests over the real Axum router.
//!
//! All upstream interaction uses the deterministic mock server; no test reaches
//! the real CodeArts portal or STS.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::account_service::AccountService;
use crate::accounts::NewAccount;
use crate::api::ModelCatalog;
use crate::checkin::{CheckinService, MockCheckinExecutor, MockQuotaRefreshHook};
use crate::config::{Config, ProviderKind, Region};
use crate::providers::qoder::auth::QoderAuthClient;
use crate::providers::trae::auth::TraeAuthClient;
use crate::providers::workbuddy::auth::WorkbuddyAuthClient;
use crate::providers::ProviderRegistry;
use crate::security::SecretBox;
use crate::storage::Storage;
use crate::test_support::{spawn_server, Reply, Server};

use super::{router, AdminState};

const KEY: &str = "admin-test-key-0123456789";
const PASSWORD: &str = "correct horse battery staple";

struct Harness {
    _dir: TempDir,
    app: Router,
    state: AdminState,
    storage: Storage,
    secret_box: Arc<SecretBox>,
}

async fn harness(sts_url: &str, flow_ttl_secs: u64) -> Harness {
    harness_with(sts_url, flow_ttl_secs, false).await
}

async fn harness_with(sts_url: &str, flow_ttl_secs: u64, secure_cookie: bool) -> Harness {
    let dir = TempDir::new().expect("temp dir");
    let secure = if secure_cookie {
        "secure_cookie = true"
    } else {
        ""
    };
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]
admin_session_ttl_hours = 1

[logging]
level = "error"

[admin]
redirect_url = "http://127.0.0.1:8787/oauth/callback"
flow_ttl_secs = {flow_ttl_secs}
codearts_cn_portal_url = "{sts_url}"
codearts_intl_portal_url = "{sts_url}"
codearts_sts_url = "{sts_url}"
{secure}

[[providers]]
kind = "mock"
region = "cn"
enabled = true
"#
    );
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).expect("write config");
    let config = Config::load_with_env(Some(&path), Some(dir.path().to_path_buf()), |_| None)
        .expect("config");
    let storage = Storage::open_file(dir.path().join("polygate.db"), 4)
        .await
        .expect("storage");
    let secret_box = Arc::new(SecretBox::from_master_key(&[9u8; 32]).expect("key"));
    let accounts = AccountService::new(storage.clone(), Arc::clone(&secret_box));
    let registry = Arc::new(ProviderRegistry::new());
    let checkin = CheckinService::new(
        storage.clone(),
        accounts.clone(),
        Arc::new(MockCheckinExecutor::new()),
        Arc::new(MockQuotaRefreshHook::new()),
    );
    let state = AdminState::new(
        &config,
        8787,
        storage.clone(),
        accounts,
        registry,
        Arc::new(ModelCatalog::new(Vec::new())),
        checkin,
        Arc::clone(&secret_box),
    );
    Harness {
        _dir: dir,
        app: router(state.clone()),
        state,
        storage,
        secret_box,
    }
}

fn peer_loopback() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 45678)))
}

fn peer_remote() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 45678)))
}

/// Rebuild the harness with a mock Qoder OpenAPI/website. CodeArts still points
/// at the same server, which is harmless because these tests never start a
/// CodeArts flow.
async fn harness_qoder(base: &str) -> Harness {
    let mut harness = harness(base, 600).await;
    harness.state.qoder_auth =
        QoderAuthClient::with_base_urls(harness.state.http.clone(), base, base);
    harness.app = router(harness.state.clone());
    harness
}

/// Rebuild the harness with a mock TRAE OAuth host.
async fn harness_trae(base: &str) -> Harness {
    let mut harness = harness(base, 600).await;
    harness.state.trae_auth = TraeAuthClient::with_base(harness.state.http.clone(), base);
    harness.app = router(harness.state.clone());
    harness
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    headers: Vec<(&str, String)>,
    body: Option<Value>,
    peer: ConnectInfo<SocketAddr>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    send_origin(
        app,
        method,
        path,
        "http://127.0.0.1:8787",
        headers,
        body,
        peer,
    )
    .await
}

async fn send_origin(
    app: &Router,
    method: Method,
    path: &str,
    origin: &str,
    headers: Vec<(&str, String)>,
    body: Option<Value>,
    peer: ConnectInfo<SocketAddr>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:8787")
        .header(header::ORIGIN, origin);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .extension(peer)
        .body(match body {
            Some(value) => Body::from(serde_json::to_vec(&value).unwrap()),
            None => Body::empty(),
        })
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec();
    (status, headers, bytes)
}

fn json_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

fn session_cookie(headers: &HeaderMap) -> String {
    headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .expect("session cookie")
        .to_string()
}

struct Session {
    cookie: String,
    csrf: String,
}

async fn setup(app: &Router) -> (Session, String) {
    let (status, headers, body) = send(
        app,
        Method::POST,
        "/admin/api/setup",
        vec![("content-type", "application/json".into())],
        Some(json!({"password": PASSWORD})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let body = json_body(&body);
    let session = Session {
        cookie: session_cookie(&headers),
        csrf: body["csrf_token"].as_str().expect("csrf").to_string(),
    };
    let recovery = body["recovery_code"]
        .as_str()
        .expect("recovery")
        .to_string();
    (session, recovery)
}

async fn authed(
    app: &Router,
    session: &Session,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let headers = vec![
        ("cookie", session.cookie.clone()),
        ("x-csrf-token", session.csrf.clone()),
        ("content-type", "application/json".into()),
    ];
    let (status, _, bytes) = send(app, method, path, headers, body, peer_loopback()).await;
    (status, json_body(&bytes))
}

async fn authed_get(app: &Router, session: &Session, path: &str) -> (StatusCode, Value) {
    let headers = vec![("cookie", session.cookie.clone())];
    let (status, _, bytes) = send(app, Method::GET, path, headers, None, peer_loopback()).await;
    (status, json_body(&bytes))
}

async fn sts_server() -> Server {
    spawn_server(|request| match (request.method.as_str(), request.path()) {
        ("POST", "/v1/oauth2/tokens") => {
            if request.body.contains("code=good-code") {
                Reply::json(
                    200,
                    json!({
                        "credentials": {
                            "access_key_id": "AK-SECRET",
                            "secret_access_key": "SK-SECRET",
                            "security_token": "STS-SECRET",
                            "expiration": "2030-01-01T00:00:00Z"
                        },
                        "refresh_token": "REFRESH-SECRET"
                    })
                    .to_string(),
                )
            } else {
                Reply::json(401, r#"{"error":"invalid_grant"}"#)
            }
        }
        ("GET", "/v5/caller-identity") => Reply::json(
            200,
            json!({
                "account_id": "domain-1",
                "principal_id": "user-1",
                "principal_urn": "urn:huawei:iam::domain-1:user/alice"
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await
}

async fn start_flow(app: &Router, session: &Session, region: &str, name: &str) -> Value {
    let (status, body) = authed(
        app,
        session,
        Method::POST,
        "/admin/api/auth/codearts/start",
        Some(json!({"region": region, "name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn complete_flow(app: &Router, code: &str) -> String {
    let (status, _, bytes) = send(
        app,
        Method::GET,
        &format!("/oauth/callback?code={code}"),
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn first_run_setup_then_login_and_cookie_flags() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;

    let (status, _, body) = send(
        &harness.app,
        Method::GET,
        "/admin/api/bootstrap",
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["initialized"], false);

    let (session, recovery) = setup(&harness.app).await;
    assert!(recovery.starts_with("pgrec-"));
    let (status, _, body) = send(
        &harness.app,
        Method::GET,
        "/admin/api/bootstrap",
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["initialized"], true);

    let (status, headers, body) = send(
        &harness.app,
        Method::POST,
        "/admin/api/login",
        vec![("content-type", "application/json".into())],
        Some(json!({"username": "admin", "password": PASSWORD})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let cookie = headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/admin"));
    assert!(!cookie.contains("Secure"));

    let (status, body) = authed_get(&harness.app, &session, "/admin/api/session").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "admin");
}

#[tokio::test]
async fn wrong_password_is_generic_and_rate_limited() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    setup(&harness.app).await;

    for _ in 0..5 {
        let (status, _, body) = send(
            &harness.app,
            Method::POST,
            "/admin/api/login",
            vec![("content-type", "application/json".into())],
            Some(json!({"username": "admin", "password": "definitely wrong password"})),
            peer_loopback(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!String::from_utf8_lossy(&body).contains(PASSWORD));
    }
    let (status, headers, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/login",
        vec![("content-type", "application/json".into())],
        Some(json!({"username": "admin", "password": PASSWORD})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn admin_api_requires_session_and_csrf() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;

    let (status, _, _) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts",
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A gateway API key must never grant admin access.
    let (status, _, _) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts",
        vec![("authorization", format!("Bearer {KEY}"))],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (session, _) = setup(&harness.app).await;

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/accounts",
        vec![
            ("cookie", session.cookie.clone()),
            ("content-type", "application/json".into()),
        ],
        Some(json!({"region": "cn", "name": "No CSRF"})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/accounts",
        vec![
            ("cookie", session.cookie.clone()),
            ("x-csrf-token", "wrong".into()),
            ("content-type", "application/json".into()),
        ],
        Some(json!({"region": "cn", "name": "Wrong CSRF"})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setup_is_limited_to_loopback() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/setup",
        vec![("content-type", "application/json".into())],
        Some(json!({"password": PASSWORD})),
        peer_remote(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/setup",
        vec![("content-type", "application/json".into())],
        Some(json!({"password": "short"})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn setup_behind_a_same_host_proxy_is_still_rejected() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/admin/api/setup")
        .header(header::HOST, "polygate.example.com")
        .header(header::ORIGIN, "https://polygate.example.com")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(peer_loopback())
        .body(Body::from(
            serde_json::to_vec(&json!({"password": PASSWORD})).unwrap(),
        ))
        .unwrap();
    let response = harness.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn codearts_authorization_creates_one_encrypted_account() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_flow(&harness.app, &session, "cn", "Primary").await;
    let flow_id = flow["flow_id"].as_str().expect("flow id").to_string();
    let authorize = flow["authorize_url"].as_str().expect("authorize url");
    assert!(authorize.contains("/portal/authorize?"));
    assert!(authorize.contains("client_id=codearts-snap"));
    assert!(authorize.contains("code_challenge_method=SHA-256"));
    assert!(
        authorize.contains("auth_callback_url=http%3A%2F%2F127.0.0.1%3A8787%2Foauth%2Fcallback")
    );

    let page = complete_flow(&harness.app, "good-code").await;
    assert!(page.contains("data-success=\"true\""));

    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/codearts/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "succeeded");
    let account_id = body["account_id"].as_str().expect("account id").to_string();

    let (status, body) = authed_get(
        &harness.app,
        &session,
        "/admin/api/accounts?kind=codearts&region=cn",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    assert_eq!(body["items"][0]["id"], account_id);
    assert_eq!(body["items"][0]["credential"]["present"], true);
    assert_eq!(body["items"][0]["auth_status"], "valid");
    assert_eq!(body["items"][0]["external_id"], "codearts:cn:user-1");

    let requests = upstream.requests();
    let token = requests
        .iter()
        .find(|request| request.path() == "/v1/oauth2/tokens")
        .expect("token request");
    assert!(token.header("DPoP").is_some());
    assert!(token.body.contains("code_verifier"));
    assert!(token.body.contains("redirect_uri"));
    let identity = requests
        .iter()
        .find(|request| request.path() == "/v5/caller-identity")
        .expect("identity request");
    assert!(identity
        .header("Authorization")
        .is_some_and(|value| value.starts_with("SDK-HMAC-SHA256")));

    let raw = harness
        .storage
        .credentials()
        .raw_payload(&account_id)
        .await
        .unwrap()
        .expect("ciphertext");
    assert!(raw.starts_with("v1:"));
    assert!(!raw.contains("SK-SECRET"));
    assert!(!raw.contains("STS-SECRET"));
    let stored = harness
        .storage
        .credentials()
        .load(&harness.secret_box, &account_id)
        .await
        .unwrap()
        .expect("credential");
    assert_eq!(stored.get("domain_id"), Some("domain-1"));
    assert!(stored.get("code_verifier").is_some());
    assert!(stored.to_json().get("dpop_private_jwk").is_some());
}

#[tokio::test]
async fn failed_authorization_creates_no_account() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    let flow = start_flow(&harness.app, &session, "cn", "Broken").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();

    let page = complete_flow(&harness.app, "bad-code").await;
    assert!(page.contains("data-success=\"false\""));

    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/codearts/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "failed");
    assert!(!body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("SK-SECRET"));

    let (_, body) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn codearts_paste_completes_a_flow_for_a_remote_browser() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_flow(&harness.app, &session, "cn", "Remote").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/codearts/{flow_id}/complete"),
        Some(json!({
            "url": "http://127.0.0.1:8787/oauth/callback?code=good-code&state=opaque"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "succeeded");
    assert!(body["account_id"].as_str().is_some());

    // A bare code is accepted too.
    let flow = start_flow(&harness.app, &session, "cn", "Remote bare").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/codearts/{flow_id}/complete"),
        Some(json!({"url": "good-code"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "succeeded");

    let (_, body) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    assert_eq!(body["count"], 1, "same identity must not duplicate");
}

#[tokio::test]
async fn codearts_paste_rejects_a_url_without_a_code() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    let flow = start_flow(&harness.app, &session, "cn", "No code").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();

    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/codearts/{flow_id}/complete"),
        Some(json!({"url": "http://127.0.0.1:8787/oauth/callback?state=only"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("code"));

    let (_, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/codearts/{flow_id}"),
    )
    .await;
    assert_eq!(
        body["status"], "pending",
        "a bad paste must not burn the flow"
    );
}

#[tokio::test]
async fn authorization_can_be_cancelled_and_expires() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 1).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_flow(&harness.app, &session, "cn", "Cancel me").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/codearts/{flow_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "cancelled");
    complete_flow(&harness.app, "good-code").await;

    let flow = start_flow(&harness.app, &session, "cn", "Timeout").await;
    let flow_id = flow["flow_id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/codearts/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "expired");
    let (_, body) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn reauthorizing_updates_the_same_identity_instead_of_duplicating() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    start_flow(&harness.app, &session, "cn", "First").await;
    complete_flow(&harness.app, "good-code").await;
    let (_, first) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    let first_id = first["items"][0]["id"].as_str().unwrap().to_string();
    let first_revision = first["items"][0]["credential"]["revision"]
        .as_i64()
        .unwrap();

    start_flow(&harness.app, &session, "cn", "Second").await;
    complete_flow(&harness.app, "good-code").await;
    let (_, second) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    assert_eq!(second["count"], 1);
    assert_eq!(second["items"][0]["id"], first_id);
    assert!(
        second["items"][0]["credential"]["revision"]
            .as_i64()
            .unwrap()
            > first_revision
    );
}

#[tokio::test]
async fn cn_and_intl_authorizations_stay_isolated() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    start_flow(&harness.app, &session, "cn", "CN").await;
    complete_flow(&harness.app, "good-code").await;
    start_flow(&harness.app, &session, "intl", "Intl").await;
    complete_flow(&harness.app, "good-code").await;

    let (_, cn) = authed_get(&harness.app, &session, "/admin/api/accounts?region=cn").await;
    let (_, intl) = authed_get(&harness.app, &session, "/admin/api/accounts?region=intl").await;
    assert_eq!(cn["count"], 1);
    assert_eq!(intl["count"], 1);
    assert_ne!(cn["items"][0]["id"], intl["items"][0]["id"]);
    assert_eq!(cn["items"][0]["external_id"], "codearts:cn:user-1");
    assert_eq!(intl["items"][0]["external_id"], "codearts:intl:user-1");
}

#[tokio::test]
async fn account_crud_enable_disable_and_delete_confirmation() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let (status, account) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/accounts",
        Some(json!({"region": "cn", "name": "Placeholder"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(account["credential"]["present"], false);
    assert_eq!(account["auth_status"], "unknown");
    let id = account["id"].as_str().unwrap().to_string();

    let (status, account) = authed(
        &harness.app,
        &session,
        Method::PATCH,
        &format!("/admin/api/accounts/{id}"),
        Some(json!({"name": "Renamed", "enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(account["name"], "Renamed");
    assert_eq!(account["enabled"], false);

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::DELETE,
        &format!("/admin/api/accounts/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = authed(
        &harness.app,
        &session,
        Method::DELETE,
        &format!("/admin/api/accounts/{id}?confirm={id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], true);
    let (_, accounts) = authed_get(&harness.app, &session, "/admin/api/accounts").await;
    assert_eq!(accounts["count"], 0);
}

#[tokio::test]
async fn unknown_quota_is_reported_as_unknown() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let (_, account) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/accounts",
        Some(json!({"region": "cn", "name": "No quota"})),
    )
    .await;
    let id = account["id"].as_str().unwrap().to_string();

    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/accounts/{id}/quota"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["unknown"], true);
    assert!(body["quota"].is_null());

    let (status, body) = authed_get(&harness.app, &session, "/admin/api/dashboard").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["quota"]["unknown"], 1);
    assert_eq!(body["quota"]["known"], 0);
}

#[tokio::test]
async fn empty_dashboard_has_zero_counts_and_no_fake_data() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let (status, body) = authed_get(&harness.app, &session, "/admin/api/dashboard").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accounts"]["total"], 0);
    assert_eq!(body["accounts"]["available"], 0);
    assert_eq!(body["quota"]["known"], 0);
    assert_eq!(body["requests"]["total"], 0);
    assert!(body["checkin"]["latest"].is_null());

    let (status, body) = authed_get(&harness.app, &session, "/admin/api/models").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn account_without_credential_never_reports_authenticated() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    let (_, account) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/accounts",
        Some(json!({"region": "cn", "name": "Uncredentialed"})),
    )
    .await;
    let id = account["id"].as_str().unwrap().to_string();
    assert_eq!(account["auth_status"], "unknown");
    assert_eq!(account["credential"]["present"], false);

    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/accounts/{id}/auth-status"),
        Some(json!({"probe": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no credential"));

    let (_, body) = authed_get(&harness.app, &session, "/admin/api/dashboard").await;
    assert_eq!(body["accounts"]["available"], 0);
}

#[tokio::test]
async fn account_and_credential_creation_rolls_back_on_unique_conflict() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;

    let mut first = NewAccount {
        provider: "codearts-cn".into(),
        kind: ProviderKind::Codearts,
        region: Some(Region::Cn),
        name: "First".into(),
        credential: json!({"access_key_id": "AK", "secret_access_key": "SK"}),
        external_id: Some("codearts:cn:user-1".into()),
        weight: 1,
        max_concurrency: 4,
        quota_group: None,
        enabled: true,
        metadata: json!({}),
    };
    harness
        .storage
        .create_account(&harness.secret_box, &first)
        .await
        .expect("first insert");

    first.name = "Duplicate".into();
    first.credential = json!({"access_key_id": "AK2", "secret_access_key": "SK2"});
    let error = harness
        .storage
        .create_account(&harness.secret_box, &first)
        .await
        .expect_err("duplicate external id must fail");
    assert_eq!(error.kind, crate::error::ErrorKind::Storage);

    let accounts = harness.storage.accounts().list().await.unwrap();
    assert_eq!(accounts.len(), 1);
    let credential = harness
        .storage
        .credentials()
        .load(&harness.secret_box, &accounts[0].id)
        .await
        .unwrap()
        .expect("original credential");
    assert_eq!(credential.get("access_key_id"), Some("AK"));
}

#[tokio::test]
async fn origin_mismatch_is_rejected_even_with_a_valid_csrf_token() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    let (status, _, _) = send_origin(
        &harness.app,
        Method::POST,
        "/admin/api/accounts",
        "https://evil.example",
        vec![
            ("cookie", session.cookie.clone()),
            ("x-csrf-token", session.csrf.clone()),
            ("content-type", "application/json".into()),
        ],
        Some(json!({"region": "cn", "name": "Evil"})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn secure_cookie_is_emitted_when_configured() {
    let upstream = sts_server().await;
    let harness = harness_with(&upstream.base_url, 600, true).await;
    let (_, headers, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/setup",
        vec![("content-type", "application/json".into())],
        Some(json!({"password": PASSWORD})),
        peer_loopback(),
    )
    .await;
    let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(cookie.contains("Secure"));
}

#[tokio::test]
async fn embedded_ui_is_served_with_security_headers() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (status, headers, body) = send(
        &harness.app,
        Method::GET,
        "/admin/",
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("PolyGate"));
    let csp = headers
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(headers
        .get(header::X_FRAME_OPTIONS)
        .is_some_and(|value| value == "DENY"));

    let (status, headers, body) = send(
        &harness.app,
        Method::GET,
        "/admin/static/app.js",
        vec![],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.to_str().unwrap().contains("javascript")));
    assert!(String::from_utf8_lossy(&body).contains("polygate"));
}

#[tokio::test]
async fn account_json_never_contains_credentials() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    start_flow(&harness.app, &session, "cn", "No leak").await;
    complete_flow(&harness.app, "good-code").await;
    let headers = vec![("cookie", session.cookie.clone())];
    let (status, _, bytes) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts",
        headers,
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8_lossy(&bytes);
    for secret in ["AK-SECRET", "SK-SECRET", "STS-SECRET", "REFRESH-SECRET"] {
        assert!(!body.contains(secret), "response leaked {secret}");
    }
}

#[tokio::test]
async fn unimplemented_provider_cannot_be_added() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/accounts",
        Some(json!({"kind": "qoder", "region": "cn", "name": "Qoder"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["error"]["code"], "unsupported_capability");
}

#[tokio::test]
async fn recovery_code_rotates_password_and_revokes_sessions() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, recovery) = setup(&harness.app).await;

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/recover",
        vec![("content-type", "application/json".into())],
        Some(json!({
            "recovery_code": recovery,
            "new_password": "a-very-long-admin-passphrase-2026"
        })),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = authed_get(&harness.app, &session, "/admin/api/session").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/login",
        vec![("content-type", "application/json".into())],
        Some(json!({"username": "admin", "password": PASSWORD})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        "/admin/api/login",
        vec![("content-type", "application/json".into())],
        Some(json!({"username": "admin", "password": "a-very-long-admin-passphrase-2026"})),
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let _ = harness.state;
}

// ---------------------------------------------------------------------------
// Qoder device authorization
// ---------------------------------------------------------------------------

async fn qoder_server(pending_polls: usize) -> Server {
    let polls = Arc::new(AtomicUsize::new(0));
    spawn_server(
        move |request| match (request.method.as_str(), request.path()) {
            ("GET", "/api/v1/deviceToken/poll") => {
                if polls.fetch_add(1, Ordering::SeqCst) < pending_polls {
                    Reply::json(202, "{}")
                } else {
                    Reply::json(
                        200,
                        json!({
                            "token": "dt-access",
                            "refresh_token": "drt-refresh",
                            "expires_in": 3_600_000,
                            "user_id": "uid-1"
                        })
                        .to_string(),
                    )
                }
            }
            ("GET", "/api/v1/userinfo") => Reply::json(
                200,
                json!({
                    "id": "uid-1",
                    "name": "Alice",
                    "user_type": "personal_professional_trial"
                })
                .to_string(),
            ),
            ("POST", "/api/v1/deviceToken/refresh") => Reply::json(
                200,
                json!({"token": "dt-new", "expires_in": 7_200_000}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        },
    )
    .await
}

async fn start_qoder_flow(app: &Router, session: &Session, region: &str, name: &str) -> Value {
    let (status, body) = authed(
        app,
        session,
        Method::POST,
        "/admin/api/auth/qoder/start",
        Some(json!({"region": region, "name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn qoder_device_flow_persists_one_account_without_leaking_tokens() {
    let upstream = qoder_server(1).await;
    let harness = harness_qoder(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_qoder_flow(&harness.app, &session, "cn", "Qoder Main").await;
    assert_eq!(flow["status"], "pending");
    assert_eq!(flow["region"], "cn");
    let authorize_url = flow["authorize_url"].as_str().expect("authorize url");
    assert!(authorize_url.contains("/device/selectAccounts"));
    assert!(authorize_url.contains("challenge_method=S256"));
    assert!(!authorize_url.contains("redirect_uri"));
    let flow_id = flow["flow_id"].as_str().expect("flow id").to_string();

    let (status, first) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["status"], "pending");
    assert!(first.get("account_id").is_none());

    let (status, completed) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["status"], "succeeded");
    let account_id = completed["account_id"]
        .as_str()
        .expect("account id")
        .to_string();
    let text = completed.to_string();
    for secret in ["dt-access", "drt-refresh"] {
        assert!(!text.contains(secret), "flow response leaked {secret}");
    }

    let account = harness
        .state
        .accounts
        .get(&account_id)
        .await
        .expect("account")
        .expect("account exists");
    assert_eq!(account.kind, ProviderKind::Qoder);
    assert_eq!(account.region, Region::Cn);
    assert_eq!(account.name, "Qoder Main");
    assert_eq!(account.external_id.as_deref(), Some("qoder:cn:uid-1"));
    assert_eq!(account.metadata["uid"], "uid-1");
    assert_eq!(account.metadata["auth_source"], "oauth");
    assert_eq!(account.auth_status.as_str(), "valid");

    let credential = harness
        .state
        .accounts
        .credentials()
        .load(&account_id)
        .await
        .expect("credential")
        .expect("credential exists");
    assert_eq!(credential.get("access_token"), Some("dt-access"));
    assert_eq!(credential.get("refresh_token"), Some("drt-refresh"));
    assert_eq!(credential.get("uid"), Some("uid-1"));
    assert_eq!(credential.get("realm"), Some("cn"));

    let row = harness
        .storage
        .admin()
        .flow(&flow_id)
        .await
        .expect("flow")
        .expect("flow row");
    assert_eq!(row.kind, "qoder");
    assert_eq!(row.redirect_uri, "");
    assert_eq!(row.status, "succeeded");
    assert_eq!(row.account_id.as_deref(), Some(account_id.as_str()));
    let plaintext = harness
        .secret_box
        .decrypt_with_aad(
            format!("polygate:admin-flow:v1:{flow_id}").as_bytes(),
            &row.secrets_encrypted,
        )
        .expect("decrypt flow secrets");
    let secrets: Value = serde_json::from_slice(&plaintext).expect("secrets json");
    let verifier = secrets["verifier"].as_str().expect("verifier");
    assert_eq!(verifier.len(), 64);
    assert_eq!(row.ticket_id, secrets["nonce"].as_str().unwrap());
    assert!(!row.secrets_encrypted.contains(verifier));

    let (status, _, bytes) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts?kind=qoder&region=cn",
        vec![("cookie", session.cookie.clone())],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = String::from_utf8_lossy(&bytes);
    assert!(!list.contains("dt-access"));
    assert!(!list.contains("drt-refresh"));
    assert!(list.contains("Alice"));
}

#[tokio::test]
async fn qoder_duplicate_flow_is_blocked_and_cancel_is_local() {
    let upstream = qoder_server(0).await;
    let harness = harness_qoder(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_qoder_flow(&harness.app, &session, "cn", "first").await;
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/qoder/start",
        Some(json!({"region": "cn", "name": "second"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request_error");

    let flow_id = flow["flow_id"].as_str().unwrap();
    let (status, cancelled) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/qoder/{flow_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(upstream.request_count(), 0);

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/qoder/start",
        Some(json!({"region": "intl", "name": "after cancel"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn qoder_cn_and_intl_accounts_stay_independent() {
    let upstream = qoder_server(0).await;
    let harness = harness_qoder(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    for region in ["cn", "intl"] {
        let flow = start_qoder_flow(&harness.app, &session, region, region).await;
        let flow_id = flow["flow_id"].as_str().unwrap();
        let (status, completed) = authed_get(
            &harness.app,
            &session,
            &format!("/admin/api/auth/qoder/{flow_id}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{completed}");
        assert_eq!(completed["status"], "succeeded");
    }

    let accounts = harness.state.accounts.list().await.expect("accounts");
    assert_eq!(accounts.len(), 2);
    let mut identities: Vec<(Region, String)> = accounts
        .into_iter()
        .map(|account| (account.region, account.external_id.unwrap_or_default()))
        .collect();
    identities.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    assert_eq!(identities[0], (Region::Cn, "qoder:cn:uid-1".into()));
    assert_eq!(identities[1], (Region::Intl, "qoder:intl:uid-1".into()));
}

#[tokio::test]
async fn qoder_login_without_uid_fails_the_flow() {
    let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
        ("GET", "/api/v1/deviceToken/poll") => {
            Reply::json(200, json!({"token": "dt-access"}).to_string())
        }
        _ => Reply::json(500, "{}"),
    })
    .await;
    let harness = harness_qoder(&server.base_url).await;
    let (session, _) = setup(&harness.app).await;
    let flow = start_qoder_flow(&harness.app, &session, "cn", "no uid").await;
    let flow_id = flow["flow_id"].as_str().unwrap();

    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, view) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["status"], "failed");
    assert!(harness.state.accounts.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn qoder_transient_poll_failure_keeps_the_flow_pending() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&polls);
    let server = spawn_server(
        move |request| match (request.method.as_str(), request.path()) {
            ("GET", "/api/v1/deviceToken/poll") => match counter.fetch_add(1, Ordering::SeqCst) {
                0 => Reply::json(503, "{}"),
                1 => Reply::json(202, "{}"),
                _ => Reply::json(
                    200,
                    json!({
                        "token": "dt-access",
                        "refresh_token": "drt-refresh",
                        "user_id": "uid-1"
                    })
                    .to_string(),
                ),
            },
            ("GET", "/api/v1/userinfo") => Reply::json(200, json!({"id": "uid-1"}).to_string()),
            _ => Reply::json(404, "{}"),
        },
    )
    .await;
    let harness = harness_qoder(&server.base_url).await;
    let (session, _) = setup(&harness.app).await;
    let flow = start_qoder_flow(&harness.app, &session, "cn", "retry").await;
    let flow_id = flow["flow_id"].as_str().unwrap();

    let (status, body) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");

    let (status, pending) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending["status"], "pending");

    let (status, completed) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(completed["status"], "succeeded");
}

#[tokio::test]
async fn qoder_expired_session_marks_the_flow_failed() {
    let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
        ("GET", "/api/v1/deviceToken/poll") => Reply::json(403, r#"{"message":"TOKEN_EXPIRE"}"#),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let harness = harness_qoder(&server.base_url).await;
    let (session, _) = setup(&harness.app).await;
    let flow = start_qoder_flow(&harness.app, &session, "cn", "expired").await;
    let flow_id = flow["flow_id"].as_str().unwrap();

    let (status, _) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, view) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/qoder/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["status"], "failed");
}

#[tokio::test]
async fn account_checkin_endpoint_runs_once_per_day_and_requires_csrf() {
    let upstream = sts_server().await;
    let harness = harness(&upstream.base_url, 600).await;
    let (session, _) = setup(&harness.app).await;

    let account = harness
        .storage
        .create_account(
            &harness.secret_box,
            &NewAccount {
                provider: "qoder-cn".into(),
                kind: ProviderKind::Qoder,
                region: Some(Region::Cn),
                name: "Q".into(),
                credential: json!({"access_token": "dt-access", "uid": "uid-1"}),
                external_id: Some("qoder:cn:uid-1".into()),
                weight: 1,
                max_concurrency: 4,
                quota_group: None,
                enabled: true,
                metadata: json!({}),
            },
        )
        .await
        .expect("account");

    let path = format!("/admin/api/accounts/{}/checkin", account.id);
    let (status, body) = authed(&harness.app, &session, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["replayed"], false);

    let (status, again) = authed(&harness.app, &session, Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["replayed"], true);

    let headers = vec![("cookie", session.cookie.clone())];
    let (status, _, _) = send(
        &harness.app,
        Method::POST,
        &path,
        headers,
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "CSRF must be required");
}

// ---------------------------------------------------------------------------
// WorkBuddy device authorization
// ---------------------------------------------------------------------------

async fn harness_workbuddy(base: &str) -> Harness {
    let mut harness = harness(base, 600).await;
    harness.state.workbuddy_auth = WorkbuddyAuthClient::with_base(harness.state.http.clone(), base);
    // Tests poll immediately; the upstream 5 s cadence is covered separately.
    harness.state.workbuddy_poll_guard = super::PollThrottle::new(std::time::Duration::ZERO);
    harness.app = router(harness.state.clone());
    harness
}

async fn workbuddy_server(pending_polls: usize) -> Server {
    let polls = Arc::new(AtomicUsize::new(0));
    spawn_server(
        move |request| match (request.method.as_str(), request.path()) {
            ("POST", "/v2/plugin/auth/state") => Reply::json(
                200,
                json!({"code": 0, "data": {
                    "authUrl": "https://www.codebuddy.cn/plugin/login?state=s-1",
                    "state": "s-1"
                }})
                .to_string(),
            ),
            ("GET", "/v2/plugin/auth/token") => {
                if polls.fetch_add(1, Ordering::SeqCst) < pending_polls {
                    Reply::json(200, json!({"code": 11217, "msg": "waiting"}).to_string())
                } else {
                    Reply::json(
                        200,
                        json!({"code": 0, "data": {
                            "accessToken": "jwt-access",
                            "refreshToken": "refresh-1",
                            "domain": "www.codebuddy.cn",
                            "enterprise_id": "ent-1",
                            "expires_in": 3600
                        }})
                        .to_string(),
                    )
                }
            }
            ("GET", "/v2/plugin/login/account") => Reply::json(
                200,
                json!({"code": 0, "data": {"account": {"uid": "uid-1", "nickname": "Alice"}}})
                    .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        },
    )
    .await
}

async fn start_workbuddy_flow(app: &Router, session: &Session, region: &str, name: &str) -> Value {
    let (status, body) = authed(
        app,
        session,
        Method::POST,
        "/admin/api/auth/workbuddy/start",
        Some(json!({"region": region, "name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn workbuddy_device_flow_persists_one_account_without_leaking_tokens() {
    let upstream = workbuddy_server(1).await;
    let harness = harness_workbuddy(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_workbuddy_flow(&harness.app, &session, "cn", "WorkBuddy Main").await;
    assert_eq!(flow["status"], "pending");
    assert_eq!(flow["region"], "cn");
    let authorize_url = flow["authorize_url"].as_str().expect("authorize url");
    assert!(authorize_url.starts_with("https://www.codebuddy.cn/plugin/login"));
    assert!(!authorize_url.contains("jwt-access"));
    let flow_id = flow["flow_id"].as_str().expect("flow id").to_string();

    let (status, first) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/workbuddy/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["status"], "pending");

    let (status, completed) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/workbuddy/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["status"], "succeeded");
    let account_id = completed["account_id"]
        .as_str()
        .expect("account id")
        .to_string();
    let text = completed.to_string();
    for secret in ["jwt-access", "refresh-1"] {
        assert!(!text.contains(secret), "flow response leaked {secret}");
    }

    let account = harness
        .state
        .accounts
        .get(&account_id)
        .await
        .expect("account")
        .expect("account exists");
    assert_eq!(account.kind, ProviderKind::Workbuddy);
    assert_eq!(account.region, Region::Cn);
    assert_eq!(account.name, "WorkBuddy Main");
    assert_eq!(account.external_id.as_deref(), Some("workbuddy:cn:uid-1"));
    assert_eq!(account.metadata["uid"], "uid-1");
    assert_eq!(account.metadata["enterprise_id"], "ent-1");
    assert_eq!(account.metadata["auth_source"], "oauth");
    assert_eq!(account.auth_status.as_str(), "valid");

    let credential = harness
        .state
        .accounts
        .credentials()
        .load(&account_id)
        .await
        .expect("credential")
        .expect("credential exists");
    assert_eq!(credential.get("access_token"), Some("jwt-access"));
    assert_eq!(credential.get("refresh_token"), Some("refresh-1"));
    assert_eq!(credential.get("uid"), Some("uid-1"));
    assert_eq!(credential.get("region"), Some("cn"));
    assert_eq!(credential.get("domain"), Some("www.codebuddy.cn"));

    let row = harness
        .storage
        .admin()
        .flow(&flow_id)
        .await
        .expect("flow")
        .expect("flow row");
    assert_eq!(row.kind, "workbuddy");
    assert_eq!(row.redirect_uri, "");
    assert_eq!(row.status, "succeeded");
    // The upstream state is encrypted with the flow id as AAD.
    let plaintext = harness
        .secret_box
        .decrypt_with_aad(
            format!("polygate:admin-flow:v1:{flow_id}").as_bytes(),
            &row.secrets_encrypted,
        )
        .expect("decrypt flow secrets");
    let secrets: Value = serde_json::from_slice(&plaintext).expect("secrets json");
    assert_eq!(secrets["upstream_state"], "s-1");
    assert!(!row.secrets_encrypted.contains("s-1"));

    let (status, _, bytes) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts?kind=workbuddy&region=cn",
        vec![("cookie", session.cookie.clone())],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = String::from_utf8_lossy(&bytes);
    assert!(!list.contains("jwt-access"));
    assert!(!list.contains("refresh-1"));
    assert!(list.contains("WorkBuddy Main"));
}

#[tokio::test]
async fn workbuddy_duplicate_flow_is_blocked_and_cancel_is_local() {
    let upstream = workbuddy_server(0).await;
    let harness = harness_workbuddy(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let flow = start_workbuddy_flow(&harness.app, &session, "cn", "first").await;
    let after_start = upstream.request_count();
    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/workbuddy/start",
        Some(json!({"region": "cn", "name": "second"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request_error");

    let flow_id = flow["flow_id"].as_str().unwrap();
    let (status, cancelled) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/workbuddy/{flow_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(
        upstream.request_count(),
        after_start,
        "cancel must not call the upstream"
    );

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/workbuddy/start",
        Some(json!({"region": "intl", "name": "after cancel"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn workbuddy_poll_throttle_keeps_the_upstream_interval() {
    let throttle = super::PollThrottle::new(std::time::Duration::from_secs(3600));
    assert!(throttle.allow("flow-1"));
    assert!(
        !throttle.allow("flow-1"),
        "a second poll inside the interval is a no-op"
    );
    assert!(throttle.allow("flow-2"), "each flow has its own timestamp");

    let open = super::PollThrottle::new(std::time::Duration::ZERO);
    assert!(open.allow("flow-1"));
    assert!(open.allow("flow-1"));
}

// ---------------------------------------------------------------------------
// TRAE authorization
// ---------------------------------------------------------------------------

const TRAE_MACHINE: &str = "0123456789abcdef0123456789abcdef";
const TRAE_DEVICE: &str = "fedcba9876543210fedcba9876543210";
/// Last 16 characters of `machine_id + device_id`.
const TRAE_TRACE: &str = "fedcba9876543210";

async fn trae_oauth_server() -> Server {
    spawn_server(|request| {
        let reply = match (request.method.as_str(), request.path()) {
            ("POST", "/cloudide/api/v3/trae/oauth/ExchangeToken") => {
                if request.body.contains("rt-callback") {
                    Reply::json(
                        200,
                        json!({
                            "Result": {
                                "Token": "at-secret-token",
                                "RefreshToken": "rt-rotated-secret",
                                "TokenExpireAt": chrono::Utc::now().timestamp_millis() + 86_400_000,
                                "TokenExpireDuration": 86_400
                            }
                        })
                        .to_string(),
                    )
                } else {
                    Reply::json(401, r#"{"code":401,"message":"refresh token rejected"}"#)
                }
            }
            ("POST", "/cloudide/api/v3/trae/GetUserInfo") => Reply::json(
                200,
                json!({"Result": {"UserID": "uid-1", "ScreenName": "Alice"}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        };
        reply
    })
    .await
}

fn trae_start_body(region: &str, name: &str) -> Value {
    json!({
        "region": region,
        "name": name,
        "machine_id": TRAE_MACHINE,
        "device_id": TRAE_DEVICE,
    })
}

fn trae_callback(trace: &str) -> String {
    format!("http://127.0.0.1:8787/oauth/callback?refreshToken=rt-callback&loginTraceID={trace}")
}

#[tokio::test]
async fn trae_login_requires_operator_supplied_device_identity() {
    let upstream = trae_oauth_server().await;
    let harness = harness_trae(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(json!({"region": "cn", "name": "missing ids"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .expect("message")
        .contains("machineId"));

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(json!({
            "region": "cn",
            "name": "short id",
            "machine_id": "abc",
            "device_id": TRAE_DEVICE,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(trae_start_body("intl", "intl login")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);

    assert_eq!(
        upstream.request_count(),
        0,
        "starting a flow never contacts the upstream"
    );
}

#[tokio::test]
async fn trae_paste_callback_completes_without_echoing_tokens() {
    let upstream = trae_oauth_server().await;
    let harness = harness_trae(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let (status, flow) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(trae_start_body("cn", "TRAE Main")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{flow}");
    assert_eq!(flow["status"], "pending");
    let authorize_url = flow["authorize_url"].as_str().expect("authorize url");
    assert!(authorize_url.starts_with("https://www.trae.cn/authorization?"));
    assert!(authorize_url.contains("client_id=en1oxy7wnw8j9n"));
    assert!(authorize_url.contains(&format!("login_trace_id={TRAE_TRACE}")));
    assert!(authorize_url.contains(&format!("machine_id={TRAE_MACHINE}")));
    let flow_id = flow["flow_id"].as_str().expect("flow id").to_string();

    // The browser lands on the callback with the refresh token in the URL. The
    // page must neither echo it nor complete the flow on its own.
    let (status, _, bytes) = send(
        &harness.app,
        Method::GET,
        "/oauth/callback?refreshToken=rt-callback",
        Vec::new(),
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8_lossy(&bytes);
    assert!(page.contains("粘贴"), "page must instruct the operator");
    assert!(
        !page.contains("rt-callback"),
        "page leaked the refresh token"
    );
    assert_eq!(upstream.request_count(), 0);

    let (status, completed) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/trae/{flow_id}/complete"),
        Some(json!({"url": trae_callback(TRAE_TRACE)})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{completed} requests={:?}",
        upstream.requests()
    );
    assert_eq!(completed["status"], "succeeded");
    let text = completed.to_string();
    for secret in ["rt-callback", "at-secret-token", "rt-rotated-secret"] {
        assert!(!text.contains(secret), "flow response leaked {secret}");
    }
    let account_id = completed["account_id"]
        .as_str()
        .expect("account id")
        .to_string();

    let account = harness
        .state
        .accounts
        .get(&account_id)
        .await
        .expect("account")
        .expect("account exists");
    assert_eq!(account.kind, ProviderKind::Trae);
    assert_eq!(account.region, Region::Cn);
    assert_eq!(account.name, "TRAE Main");
    assert_eq!(account.external_id.as_deref(), Some("trae:cn:uid-1"));
    assert_eq!(account.metadata["uid"], "uid-1");
    assert_eq!(account.metadata["realm"], "cn");
    assert_eq!(account.metadata["auth_source"], "oauth");
    assert_eq!(account.auth_status.as_str(), "valid");

    let credential = harness
        .state
        .accounts
        .credentials()
        .load(&account_id)
        .await
        .expect("credential")
        .expect("credential exists");
    assert_eq!(credential.get("access_token"), Some("at-secret-token"));
    assert_eq!(credential.get("refresh_token"), Some("rt-rotated-secret"));
    assert_eq!(credential.get("uid"), Some("uid-1"));
    assert_eq!(credential.get("machine_id"), Some(TRAE_MACHINE));
    assert_eq!(credential.get("device_id"), Some(TRAE_DEVICE));
    assert_eq!(credential.get("region"), Some("cn"));
    assert_eq!(credential.get("auth_source"), Some("trae"));

    let row = harness
        .storage
        .admin()
        .flow(&flow_id)
        .await
        .expect("flow")
        .expect("flow row");
    assert_eq!(row.kind, "trae");
    assert_eq!(row.status, "succeeded");
    assert_eq!(row.ticket_id, TRAE_TRACE);
    assert!(!row.secrets_encrypted.contains(TRAE_MACHINE));

    let (status, _, list) = send(
        &harness.app,
        Method::GET,
        "/admin/api/accounts?kind=trae&region=cn",
        vec![("cookie", session.cookie.clone())],
        None,
        peer_loopback(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = String::from_utf8_lossy(&list);
    assert!(!list.contains("at-secret-token"));
    assert!(!list.contains("rt-rotated-secret"));
    assert!(list.contains("Alice"));
}

#[tokio::test]
async fn trae_callback_from_another_login_keeps_the_flow_pending() {
    let upstream = trae_oauth_server().await;
    let harness = harness_trae(&upstream.base_url).await;
    let (session, _) = setup(&harness.app).await;

    let (status, flow) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(trae_start_body("cn", "bound login")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let flow_id = flow["flow_id"].as_str().expect("flow id").to_string();

    let (status, body) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/trae/{flow_id}/complete"),
        Some(json!({"url": trae_callback("0000000000000000")})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .expect("message")
        .contains("does not belong"));
    assert_eq!(upstream.request_count(), 0, "no exchange for a stale paste");

    let (status, still) = authed_get(
        &harness.app,
        &session,
        &format!("/admin/api/auth/trae/{flow_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["status"], "pending");

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(trae_start_body("cn", "second")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "one flow at a time");

    let (status, cancelled) = authed(
        &harness.app,
        &session,
        Method::POST,
        &format!("/admin/api/auth/trae/{flow_id}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(upstream.request_count(), 0);

    let (status, _) = authed(
        &harness.app,
        &session,
        Method::POST,
        "/admin/api/auth/trae/start",
        Some(trae_start_body("cn", "after cancel")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
