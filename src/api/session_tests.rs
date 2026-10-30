//! Session lease lifecycle over the real Axum router (M13-D2-E).
//!
//! These tests drive the same path a client uses: gateway key → catalogue →
//! `SessionRouter::open` → Qoder adapter → OpenAI SSE, with the Qoder gateway
//! pointed at the deterministic mock HTTP server. They assert what happens to
//! the account/session permits when a stream ends early (client disconnect,
//! idle timeout, quota exhaustion) and that one downstream session never
//! overlaps itself upstream.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{router, ApiKeyAuth, ApiState, ModelCatalog, ModelEntry};
use crate::account_service::{AccountService, CreateAccount};
use crate::accounts::AuthStatus;
use crate::config::{ProviderKind, Region, Verification};
use crate::providers::qoder::provider::QoderProvider;
use crate::providers::{ProviderKey, ProviderRef, ProviderRegistry};
use crate::security::SecretBox;
use crate::session_service::{LimitPolicy, SessionBindingService, SessionKey, SessionRouter};
use crate::sessions::{Session, SessionState};
use crate::storage::Storage;
use crate::test_support::{spawn_server, Recorded, Reply};

const KEY: &str = "test-key";

/// One content chunk wrapped in the Qoder envelope, terminated by a blank line
/// so the SSE decoder emits it immediately.
const CONTENT_FRAME: &str = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"}}]}\"}\n\n";
const DONE_FRAME: &str = "data: [DONE]\n\n";
const QUOTA_FRAME: &str = "data: [EXCEED_QUOTA]\n\n";

fn complete_stream() -> Reply {
    Reply::sse(format!("{CONTENT_FRAME}{DONE_FRAME}"))
}

fn policy(max_in_flight: usize, queue_timeout: Duration) -> LimitPolicy {
    LimitPolicy {
        max_in_flight,
        max_starts: 0,
        window: Duration::ZERO,
        queue_timeout,
        serialize_session: true,
    }
}

struct RouteApp {
    app: Router,
    accounts: AccountService,
    sessions: SessionBindingService,
    account_id: String,
    _server: crate::test_support::Server,
}

async fn route_app<F>(policy: LimitPolicy, idle: Duration, handler: F) -> RouteApp
where
    F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
{
    let server = spawn_server(handler).await;
    let storage = Storage::open_memory().await.expect("memory db");
    let accounts = AccountService::new(
        storage.clone(),
        Arc::new(SecretBox::from_master_key(&[9u8; 32]).expect("master key")),
    );
    let account = accounts
        .create(CreateAccount::new(
            ProviderKind::Qoder,
            Region::Cn,
            "q1",
            Some(json!({
                "access_token": "dt-access",
                "refresh_token": "drt-refresh",
                "uid": "uid-1",
                "nickname": "N",
                "expires_at": 0,
            })),
        ))
        .await
        .expect("account");

    let provider = QoderProvider::with_base(
        "qoder-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        server.base_url.clone(),
    )
    .with_stream_idle_timeout(idle);
    let provider: ProviderRef = Arc::new(provider);
    let registry = Arc::new(ProviderRegistry::new());
    registry.register(provider).expect("register provider");

    let catalog = ModelCatalog::new(vec![ModelEntry {
        id: "qmodel".into(),
        upstream_id: "qmodel".into(),
        provider: ProviderKey::new(ProviderKind::Qoder, Region::Cn),
        display_name: Some("Qoder Model".into()),
        context_window: Some(128_000),
        max_output: None,
        supports_vision: false,
    }]);

    let sessions =
        SessionBindingService::new(storage, accounts.clone(), policy).expect("session service");
    let session_router = SessionRouter::new(sessions.clone(), accounts.clone());
    let state = ApiState::new(
        registry,
        catalog,
        ApiKeyAuth::new(vec![KEY.to_string()]).expect("api key"),
    )
    .with_router(session_router);

    RouteApp {
        app: router(state),
        accounts,
        sessions,
        account_id: account.id,
        _server: server,
    }
}

impl RouteApp {
    fn key(&self, id: &str) -> SessionKey {
        SessionKey::new(id, ProviderKind::Qoder, Region::Cn)
    }

    async fn session_row(&self, id: &str) -> Session {
        self.sessions
            .get(&self.key(id))
            .await
            .expect("read session")
            .expect("session row")
    }

    async fn auth_status(&self) -> AuthStatus {
        self.accounts
            .get(&self.account_id)
            .await
            .expect("read account")
            .expect("account")
            .auth_status
    }
}

fn chat_request(session: &str, text: &str, stream: bool) -> Request<Body> {
    let body = json!({
        "model": "qmodel",
        "messages": [{"role": "user", "content": text}],
        "stream": stream,
    });
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-session-id", session)
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("response body");
    (status, body.to_vec())
}

fn sse_frames(body: &[u8]) -> Vec<String> {
    String::from_utf8(body.to_vec())
        .expect("utf8 body")
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.trim().to_string())
        .filter(|data| !data.is_empty())
        .collect()
}

fn ends_with_done(frames: &[String]) -> bool {
    frames.last().map(String::as_str) == Some("[DONE]")
}

fn error_code(frames: &[String]) -> Option<String> {
    frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .filter_map(|frame| serde_json::from_str::<Value>(frame).ok())
        .find_map(|value| value["error"]["code"].as_str().map(str::to_string))
}

fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

// ---------------------------------------------------------------------------
// 1. Client disconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_disconnect_releases_the_session_and_account_lease() {
    let calls = counter();
    let handler = {
        let calls = Arc::clone(&calls);
        move |_: &Recorded| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::sse_stalled(
                    format!("{CONTENT_FRAME}{DONE_FRAME}"),
                    CONTENT_FRAME.len(),
                    Duration::from_secs(5),
                )
            } else {
                complete_stream()
            }
        }
    };
    // One slot and one gate: a leaked permit or lock makes the second request
    // wait for `queue_timeout` and fail with 503.
    let app = route_app(
        policy(1, Duration::from_millis(250)),
        Duration::from_secs(5),
        handler,
    )
    .await;

    let response = app
        .app
        .clone()
        .oneshot(chat_request("s1", "first", true))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let frame = body
        .frame()
        .await
        .expect("first frame")
        .expect("frame data");
    assert!(frame.data_ref().is_some());
    drop(body);

    let started = Instant::now();
    let (status, body) = send(&app.app, chat_request("s1", "second", true)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a dropped SSE body must release the account and session permits"
    );
    assert!(ends_with_done(&sse_frames(&body)));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the release must be immediate, not a queue timeout"
    );

    let row = app.session_row("s1").await;
    assert_eq!(
        row.request_count, 1,
        "only the completed request is recorded; the aborted one is not"
    );
    assert_eq!(row.state, SessionState::Active);
}

// ---------------------------------------------------------------------------
// 2. Stream idle timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn idle_timeout_ends_the_stream_and_releases_the_lease() {
    let calls = counter();
    let handler = {
        let calls = Arc::clone(&calls);
        move |_: &Recorded| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::sse_stalled(
                    format!("{CONTENT_FRAME}{DONE_FRAME}"),
                    CONTENT_FRAME.len(),
                    Duration::from_secs(5),
                )
            } else {
                complete_stream()
            }
        }
    };
    // The production idle timeout is 120 s; 150 ms exercises the same path.
    let app = route_app(
        policy(1, Duration::from_millis(250)),
        Duration::from_millis(150),
        handler,
    )
    .await;

    let response = app
        .app
        .clone()
        .oneshot(chat_request("s1", "first", true))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .expect("the idle timeout must end the SSE body")
    .expect("body");

    let frames = sse_frames(&body);
    assert!(ends_with_done(&frames));
    assert_eq!(error_code(&frames).as_deref(), Some("timeout"));

    let row = app.session_row("s1").await;
    assert_eq!(row.request_count, 1, "finish() runs for a timed-out stream");
    assert_eq!(row.state, SessionState::Active);
    assert!(row.upstream_session_id.is_some());
    assert_eq!(
        app.auth_status().await,
        AuthStatus::Unknown,
        "an idle timeout must not blame the credential"
    );

    // The same session and account slot are usable again immediately.
    let (status, body) = send(&app.app, chat_request("s1", "second", true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ends_with_done(&sse_frames(&body)));
    assert_eq!(app.session_row("s1").await.request_count, 2);
}

// ---------------------------------------------------------------------------
// 3. Concurrent requests in one session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_requests_for_one_session_are_serialized() {
    let calls = counter();
    let handler = {
        let calls = Arc::clone(&calls);
        move |_: &Recorded| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // Holds the request open for 400 ms after the first frame, so a
                // non-serialized second request would have time to overlap.
                Reply::sse_stalled(
                    format!("{CONTENT_FRAME}{DONE_FRAME}"),
                    CONTENT_FRAME.len(),
                    Duration::from_millis(400),
                )
            } else {
                complete_stream()
            }
        }
    };
    let app = route_app(
        policy(2, Duration::from_secs(3)),
        Duration::from_secs(5),
        handler,
    )
    .await;

    let first = app
        .app
        .clone()
        .oneshot(chat_request("s1", "first", true))
        .await
        .expect("first response");
    assert_eq!(first.status(), StatusCode::OK);
    let mut first_body = first.into_body();
    let _ = first_body.frame().await.expect("frame").expect("data");

    let second = {
        let app = app.app.clone();
        tokio::spawn(async move { send(&app, chat_request("s1", "second", true)).await })
    };
    let mut second = second;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut second)
            .await
            .is_err(),
        "a same-session request must wait for the held lease"
    );

    // A different session is not blocked: the gate is per session, not global.
    let (status, body) = send(&app.app, chat_request("s2", "other", true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ends_with_done(&sse_frames(&body)));

    // Draining the first body releases its lease, which wakes the waiter.
    let body = tokio::time::timeout(Duration::from_secs(5), to_bytes(first_body, 1024 * 1024))
        .await
        .expect("first body ends")
        .expect("body");
    assert!(ends_with_done(&sse_frames(&body)));

    let (status, body) = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the same-session waiter wakes")
        .expect("task");
    assert_eq!(status, StatusCode::OK);
    assert!(ends_with_done(&sse_frames(&body)));

    // Both requests of `s1` were counted on the same binding.
    assert_eq!(app.session_row("s1").await.request_count, 2);
}

// ---------------------------------------------------------------------------
// 4. Upstream quota exhaustion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quota_exhaustion_mid_stream_ends_the_stream_and_frees_the_lease() {
    let calls = counter();
    let handler = {
        let calls = Arc::clone(&calls);
        move |_: &Recorded| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::sse(format!("{CONTENT_FRAME}{QUOTA_FRAME}"))
            } else {
                complete_stream()
            }
        }
    };
    let app = route_app(
        policy(1, Duration::from_millis(250)),
        Duration::from_secs(5),
        handler,
    )
    .await;

    let (status, body) = send(&app.app, chat_request("s1", "first", true)).await;
    assert_eq!(status, StatusCode::OK);
    let frames = sse_frames(&body);
    assert!(
        frames
            .iter()
            .any(|frame| frame.contains(r#""content":"Hi""#)),
        "content before the sentinel is still delivered"
    );
    assert!(ends_with_done(&frames));
    assert_eq!(error_code(&frames).as_deref(), Some("insufficient_quota"));

    let row = app.session_row("s1").await;
    assert_eq!(row.request_count, 1);
    assert_eq!(row.state, SessionState::Active);
    assert!(
        row.upstream_session_id.is_some(),
        "a quota error must not reset the upstream session"
    );
    assert_eq!(
        app.auth_status().await,
        AuthStatus::Unknown,
        "quota exhaustion must not mark the credential invalid"
    );

    let (status, body) = send(&app.app, chat_request("s1", "second", true)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the quota-terminated stream must release its lease"
    );
    assert!(ends_with_done(&sse_frames(&body)));
}

#[tokio::test]
async fn quota_exhaustion_before_the_stream_returns_402_and_frees_the_lease() {
    let calls = counter();
    let handler = {
        let calls = Arc::clone(&calls);
        move |_: &Recorded| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::sse(format!("{CONTENT_FRAME}{QUOTA_FRAME}"))
            } else {
                complete_stream()
            }
        }
    };
    let app = route_app(
        policy(1, Duration::from_millis(250)),
        Duration::from_secs(5),
        handler,
    )
    .await;

    // Non-streaming calls aggregate the same upstream stream, so the sentinel
    // fails the request before any SSE body is produced.
    let (status, body) = send(&app.app, chat_request("s1", "first", false)).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    let value: Value = serde_json::from_slice(&body).expect("error json");
    assert_eq!(value["error"]["code"], "insufficient_quota");

    let row = app.session_row("s1").await;
    assert_eq!(row.request_count, 1);
    assert_eq!(row.state, SessionState::Active);
    assert!(row.upstream_session_id.is_some());

    let (status, body) = send(&app.app, chat_request("s1", "second", false)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a pre-stream quota failure must release its lease"
    );
    let value: Value = serde_json::from_slice(&body).expect("completion json");
    assert_eq!(value["choices"][0]["message"]["content"], "Hi");
    assert_eq!(app.session_row("s1").await.request_count, 2);
}
