//! CodeArts adapter tests: deterministic mock HTTP server + sanitized fixtures.
//!
//! No test opens a real network connection. The server is a tiny HTTP/1.1
//! implementation on `127.0.0.1:0` that records exact requests, so signature
//! headers, body shape and context propagation are asserted without a real
//! upstream.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::accounts::Credential;
use crate::config::{Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::{ChatMessage, ChatRequest, StreamEvent};
use crate::providers::{
    CodeArtsHttp, CodeArtsProvider, CredentialStore, Provider, ProviderContext, UpstreamModel,
};
use crate::test_support::{spawn_server, Recorded, Reply};

const MODELS_FIXTURE: &str = include_str!("fixtures/models.json");
const QUOTA_FIXTURE: &str = include_str!("fixtures/quota.json");
const SSE_FIXTURE: &str = include_str!("fixtures/chat_sse.txt");
const CONCURRENCY_FIXTURE: &str = include_str!("fixtures/error_concurrency.json");

fn expect_error(
    result: Result<crate::providers::ChatStream, crate::error::GatewayError>,
) -> crate::error::GatewayError {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn credential(expiration: Option<&str>) -> Credential {
    let mut value = json!({
        "access_key_id": "AKID",
        "secret_access_key": "SECRET-KEY",
        "security_token": "STS-TOKEN",
        "domain_id": "domain-1",
    });
    if let Some(expiration) = expiration {
        value["expiration"] = json!(expiration);
    }
    Credential::from_value(value)
}

fn refreshable_credential() -> Credential {
    Credential::from_value(json!({
        "access_key_id": "AKID",
        "secret_access_key": "SECRET-KEY",
        "security_token": "STS-TOKEN",
        "domain_id": "domain-1",
        "expiration": "2020-01-01T00:00:00Z",
        "refresh_token": "OLD-REFRESH",
        "code_verifier": "verifier",
        "dpop_private_jwk": {
            "kty": "EC",
            "crv": "P-256",
            "d": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE",
            "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE",
            "y": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE"
        }
    }))
}

fn codearts_provider(base_url: &str) -> CodeArtsProvider {
    let http = CodeArtsHttp::new(base_url, base_url)
        .expect("http")
        .with_timeouts(Duration::from_secs(2), Duration::from_secs(2));
    provider_with(http, None)
}

fn provider_with(http: CodeArtsHttp, store: Option<Arc<dyn CredentialStore>>) -> CodeArtsProvider {
    let provider = CodeArtsProvider::new(
        "codearts-cn",
        Region::Cn,
        Verification::Experimental,
        http.base_url().to_string(),
        http.sts_url().to_string(),
    )
    .expect("provider")
    .with_http(http);
    match store {
        Some(store) => provider.with_credentials_store(store),
        None => provider,
    }
}

fn request() -> ChatRequest {
    let mut request = ChatRequest::new(
        "glm-5.2",
        vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ],
    );
    request.max_output_tokens = Some(256);
    request.temperature = Some(0.1);
    request
}

fn context() -> ProviderContext {
    ProviderContext {
        account_id: Some("account-1".into()),
        credential_revision: Some(7),
        client_session_id: Some("client-session-1".into()),
        upstream_session_id: Some("0123456789abcdef0123456789abcdef".into()),
    }
}

fn basic_handler() -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    |request: &Recorded| match request.path() {
        "/v1/model/builtin" => Reply::json(200, MODELS_FIXTURE),
        "/snap-manager/v1/statistics/plugin" => Reply::json(200, QUOTA_FIXTURE),
        "/api/v2/chat/completions" => Reply::sse(SSE_FIXTURE),
        _ => Reply::json(404, "{}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_status_valid_and_rejected() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    assert_eq!(
        provider
            .auth_status(&credential(None))
            .await
            .expect("valid"),
        crate::accounts::AuthStatus::Valid
    );

    let unauthorized = spawn_server(|_| Reply::json(401, "APIG.0301 decrypt token fail")).await;
    let provider = codearts_provider(&unauthorized.base_url);
    assert_eq!(
        provider
            .auth_status(&credential(None))
            .await
            .expect("invalid"),
        crate::accounts::AuthStatus::Invalid
    );
}

#[tokio::test]
async fn expired_credential_is_detected_without_a_request() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    assert_eq!(
        provider
            .auth_status(&credential(Some("2020-01-01T00:00:00Z")))
            .await
            .expect("expired"),
        crate::accounts::AuthStatus::Expired
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn model_discovery_uses_the_builtin_endpoint() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    let models: Vec<UpstreamModel> = provider
        .list_models(&credential(None))
        .await
        .expect("models");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "glm-5.2");
    assert_eq!(models[1].id, "openpangu");

    let requests = server.requests();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path(), "/v1/model/builtin");
    assert_eq!(requests[0].header("Agent-Type"), Some("PromptCenter"));
    assert!(requests[0]
        .header("Authorization")
        .expect("authorization")
        .starts_with("SDK-HMAC-SHA256 Access=AKID"));
    assert_eq!(requests[0].header("X-Security-Token"), Some("STS-TOKEN"));
    assert!(requests[0].header("X-Sdk-Date").is_some());
}

#[tokio::test]
async fn quota_parses_credits_and_marks_missing_data_unknown() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    let quota = provider
        .quota_for("account-1", &credential(None))
        .await
        .expect("quota");
    assert_eq!(quota.account_id, "account-1");
    assert_eq!(quota.remaining, Some(18_000.0));
    assert_eq!(quota.total, Some(30_000.0));
    assert_eq!(quota.unit, crate::quota::QuotaUnit::Credits);

    let empty = spawn_server(|_| Reply::json(200, "{\"metrics\":[]}")).await;
    let provider = codearts_provider(&empty.base_url);
    let quota = provider
        .quota_for("account-1", &credential(None))
        .await
        .expect("quota");
    assert_eq!(quota.status, crate::quota::QuotaStatus::Unknown);
    assert_eq!(quota.remaining, None);
}

#[tokio::test]
async fn chat_aggregates_the_stream_and_sends_a_stable_chat_id() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    let response = provider
        .chat_for(&context(), &credential(None), request())
        .await
        .expect("chat");

    assert_eq!(response.text(), "Hello");
    assert_eq!(
        response.finish_reason,
        crate::protocol::FinishReason::ToolCalls
    );
    let call = response.message.tool_calls().next().expect("tool call");
    assert_eq!(call.name, "read_file");
    assert_eq!(call.arguments, "{\"path\":\"README.md\"}");
    let usage = response.usage.expect("usage");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.cached_input_tokens, Some(3));

    let requests = server.requests();
    let chat: &Recorded = requests
        .iter()
        .find(|request| request.path() == "/api/v2/chat/completions")
        .expect("chat request");
    let body: Value = serde_json::from_str(&chat.body).expect("json body");
    assert_eq!(body["stream"], true);
    assert_eq!(body["chat_id"], "0123456789abcdef0123456789abcdef");
    assert_eq!(body["prompt_cache_key"], body["chat_id"]);
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(body["model"], "glm-5.2");
    assert_eq!(chat.header("x-auth-token"), Some("STS-TOKEN"));
    assert_eq!(chat.header("Accept"), Some("text/event-stream"));
    assert!(!chat.body.contains("SECRET-KEY"));
}

#[tokio::test]
async fn streaming_emits_incremental_events_with_context_session() {
    let server = spawn_server(basic_handler()).await;
    let provider = codearts_provider(&server.base_url);
    let mut stream = provider
        .chat_stream_for(&context(), &credential(None), request())
        .await
        .expect("stream");
    let mut events = Vec::new();
    use futures_util::StreamExt;
    while let Some(item) = stream.next().await {
        events.push(item.expect("event"));
    }

    assert!(matches!(events.first(), Some(StreamEvent::Start { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "Hello")));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::ReasoningDelta { text } if text == "thinking")));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallStart { id, .. } if id == "call_1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallArgsDelta { delta, .. } if delta == "{\"pa"
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::Usage { .. })));
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finish {
            reason: crate::protocol::FinishReason::ToolCalls
        })
    ));
}

#[tokio::test]
async fn concurrency_limit_error_is_retryable_and_never_leaks_the_body() {
    let server = spawn_server(|_| Reply::json(400, CONCURRENCY_FIXTURE)).await;
    let provider = codearts_provider(&server.base_url);
    let error = expect_error(
        provider
            .chat_stream_for(&context(), &credential(None), request())
            .await,
    );
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert!(error.message.contains("session limit"));
    assert!(!error.message.contains("SECRET-KEY"));
}

#[tokio::test]
async fn rate_limit_and_server_errors_are_classified() {
    let limited = spawn_server(|_| Reply::json(429, "slow down")).await;
    let rate_limited = codearts_provider(&limited.base_url);
    let error = expect_error(
        rate_limited
            .chat_stream_for(&context(), &credential(None), request())
            .await,
    );
    assert_eq!(error.kind, ErrorKind::RateLimited);

    let broken = spawn_server(|_| Reply::json(503, "maintenance")).await;
    let failing = codearts_provider(&broken.base_url);
    let error = expect_error(
        failing
            .chat_stream_for(&context(), &credential(None), request())
            .await,
    );
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn request_timeout_is_reported_as_timeout() {
    let server = spawn_server(|_| Reply::slow(Duration::from_millis(300))).await;
    let http = CodeArtsHttp::new(server.base_url.as_str(), server.base_url.as_str())
        .expect("http")
        .with_timeouts(Duration::from_millis(50), Duration::from_millis(50));
    let provider = provider_with(http, None);
    let error = expect_error(
        provider
            .chat_stream_for(&context(), &credential(None), request())
            .await,
    );
    assert_eq!(error.kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn upstream_error_bodies_are_redacted() {
    let server = spawn_server(|_| {
        Reply::json(
            400,
            "{\"x-auth-token\":\"SECRET-VALUE\",\"error_msg\":\"bad request\"}",
        )
    })
    .await;
    let provider = codearts_provider(&server.base_url);
    let error = expect_error(
        provider
            .chat_stream_for(&context(), &credential(None), request())
            .await,
    );
    assert!(!error.message.contains("SECRET-VALUE"));
    assert!(!format!("{error:?}").contains("SECRET-VALUE"));
}

// ---------------------------------------------------------------------------
// Credential refresh
// ---------------------------------------------------------------------------

struct MemoryStore {
    current: Mutex<(Credential, i64)>,
    updates: Mutex<Vec<(String, i64)>>,
    conflict: AtomicBool,
    replacement: Option<Credential>,
    token_calls: AtomicUsize,
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn update_credential(
        &self,
        account_id: &str,
        expected_revision: i64,
        credential: &Credential,
    ) -> Result<i64, crate::error::GatewayError> {
        if self.conflict.swap(false, Ordering::SeqCst) {
            let replacement = self
                .replacement
                .clone()
                .ok_or_else(|| crate::error::GatewayError::internal("no replacement"))?;
            *self.current.lock() = (replacement, expected_revision + 1);
            return Err(crate::error::GatewayError::bad_request(
                "credential was modified concurrently",
            ));
        }
        self.updates
            .lock()
            .push((account_id.to_string(), expected_revision));
        *self.current.lock() = (credential.clone(), expected_revision + 1);
        Ok(expected_revision + 1)
    }

    async fn load_credential(
        &self,
        _account_id: &str,
    ) -> Result<Option<(Credential, i64)>, crate::error::GatewayError> {
        Ok(Some(self.current.lock().clone()))
    }
}

fn refresh_handler(store: Arc<MemoryStore>) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    move |request: &Recorded| match request.path() {
        "/v1/oauth2/tokens" => {
            store.token_calls.fetch_add(1, Ordering::SeqCst);
            assert!(request.header("DPoP").is_some(), "DPoP proof is required");
            Reply::json(
                200,
                json!({
                    "credentials": {
                        "access_key_id": "AKID-2",
                        "secret_access_key": "SECRET-KEY-2",
                        "security_token": "STS-TOKEN-2",
                        "expiration": "2999-01-01T00:00:00Z"
                    },
                    "refresh_token": "NEW-REFRESH"
                })
                .to_string(),
            )
        }
        "/api/v2/chat/completions" => Reply::sse(SSE_FIXTURE),
        _ => Reply::json(404, "{}"),
    }
}

#[tokio::test]
async fn expiring_credential_is_refreshed_with_revision_cas() {
    let store = Arc::new(MemoryStore {
        current: Mutex::new((refreshable_credential(), 7)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(false),
        replacement: None,
        token_calls: AtomicUsize::new(0),
    });
    let server = spawn_server(refresh_handler(Arc::clone(&store))).await;
    let provider = provider_with(
        CodeArtsHttp::new(server.base_url.as_str(), server.base_url.as_str()).expect("http"),
        Some(Arc::clone(&store) as Arc<dyn CredentialStore>),
    );

    let response = provider
        .chat_for(&context(), &refreshable_credential(), request())
        .await
        .expect("chat after refresh");
    assert_eq!(response.text(), "Hello");
    assert_eq!(store.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.updates.lock().as_slice(),
        &[("account-1".to_string(), 7)]
    );
    let (stored, revision) = store.current.lock().clone();
    assert_eq!(revision, 8);
    assert_eq!(stored.get("refresh_token"), Some("NEW-REFRESH"));
    assert_eq!(stored.get("access_key_id"), Some("AKID-2"));

    let requests = server.requests();
    let chat = requests
        .iter()
        .find(|request| request.path() == "/api/v2/chat/completions")
        .expect("chat");
    assert!(chat
        .header("Authorization")
        .expect("authorization")
        .contains("Access=AKID-2"));
    assert_eq!(chat.header("x-auth-token"), Some("STS-TOKEN-2"));
}

#[tokio::test]
async fn refresh_cas_conflict_uses_the_newly_stored_credential() {
    let mut replacement = credential(Some("2999-01-01T00:00:00Z"));
    replacement.0["access_key_id"] = json!("AKID-3");
    let store = Arc::new(MemoryStore {
        current: Mutex::new((refreshable_credential(), 7)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(true),
        replacement: Some(replacement),
        token_calls: AtomicUsize::new(0),
    });
    let server = spawn_server(refresh_handler(Arc::clone(&store))).await;
    let provider = provider_with(
        CodeArtsHttp::new(server.base_url.as_str(), server.base_url.as_str()).expect("http"),
        Some(Arc::clone(&store) as Arc<dyn CredentialStore>),
    );

    provider
        .chat_for(&context(), &refreshable_credential(), request())
        .await
        .expect("chat after conflict");
    // Exactly one refresh attempt: the one-time refresh token is never consumed
    // twice, and the loser of the CAS uses the winner's credential.
    assert_eq!(store.token_calls.load(Ordering::SeqCst), 1);
    let requests = server.requests();
    let chat = requests
        .iter()
        .find(|request| request.path() == "/api/v2/chat/completions")
        .expect("chat");
    assert!(chat
        .header("Authorization")
        .expect("authorization")
        .contains("Access=AKID-3"));
}

#[test]
fn capability_declaration_matches_implemented_methods() {
    let provider = codearts_provider("http://127.0.0.1:1");
    let capabilities = crate::providers::Provider::capabilities(&provider);
    assert!(capabilities.authentication);
    assert!(capabilities.model_discovery);
    assert!(capabilities.quota);
    assert!(capabilities.chat);
    assert!(capabilities.streaming);
    assert!(capabilities.tools);
    assert!(!capabilities.native_session);
    assert!(!capabilities.cancellation);
    assert_eq!(
        crate::providers::Provider::info(&provider).verification,
        crate::providers::VerificationState::Experimental
    );
}

#[tokio::test]
async fn auth_probe_refreshes_an_expired_credential_once() {
    let store = Arc::new(MemoryStore {
        current: Mutex::new((refreshable_credential(), 3)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(false),
        replacement: None,
        token_calls: AtomicUsize::new(0),
    });
    let server = spawn_server({
        let store = Arc::clone(&store);
        move |request: &Recorded| match request.path() {
            "/v1/oauth2/tokens" => {
                store.token_calls.fetch_add(1, Ordering::SeqCst);
                Reply::json(
                    200,
                    json!({
                        "credentials": {
                            "access_key_id": "AKID-2",
                            "secret_access_key": "SECRET-KEY-2",
                            "security_token": "STS-TOKEN-2",
                            "expiration": "2999-01-01T00:00:00Z"
                        },
                        "refresh_token": "NEW-REFRESH"
                    })
                    .to_string(),
                )
            }
            "/v1/model/builtin" => Reply::json(200, json!({"models": []}).to_string()),
            _ => Reply::json(404, "{}"),
        }
    })
    .await;
    let provider = provider_with(
        CodeArtsHttp::new(server.base_url.as_str(), server.base_url.as_str()).expect("http"),
        Some(Arc::clone(&store) as Arc<dyn CredentialStore>),
    );

    let status = provider
        .auth_status_with_context(&context(), &refreshable_credential())
        .await
        .expect("probe");
    assert_eq!(status, crate::accounts::AuthStatus::Valid);
    assert_eq!(store.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.updates.lock().as_slice(),
        &[("account-1".to_string(), 3)]
    );
    let (stored, revision) = store.current.lock().clone();
    assert_eq!(revision, 4);
    assert_eq!(stored.get("access_key_id"), Some("AKID-2"));
}

#[tokio::test]
async fn auth_probe_without_refresh_material_reports_expired() {
    let mut expired = refreshable_credential();
    expired
        .0
        .as_object_mut()
        .expect("object")
        .remove("refresh_token");
    expired
        .0
        .as_object_mut()
        .expect("object")
        .remove("code_verifier");
    expired
        .0
        .as_object_mut()
        .expect("object")
        .remove("dpop_private_jwk");
    let provider = codearts_provider("http://127.0.0.1:9");
    let status = provider
        .auth_status_with_context(&context(), &expired)
        .await
        .expect("probe");
    assert_eq!(status, crate::accounts::AuthStatus::Expired);
}
