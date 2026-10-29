//! WorkBuddy adapter tests. Every test talks to a local mock server; none
//! reaches a real upstream or holds a real credential.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::accounts::Credential;
use crate::config::{Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{
    ChatMessage, ChatRequest, ContentBlock, FinishReason, MessageRole, ToolResult,
};
use crate::providers::{CredentialStore, Provider, ProviderContext};
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::WorkbuddyProvider;

const CN_MODELS: &str = "/v2/enterprises/ent-1/models";
const INTL_CONFIG: &str = "/v3/config";
const CHAT: &str = "/v2/chat/completions";

fn cn_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-access",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 0
    }))
}

fn intl_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-intl",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.workbuddy.ai",
        "expires_at": 0
    }))
}

fn provider(base: &str) -> WorkbuddyProvider {
    WorkbuddyProvider::with_base(
        "workbuddy-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn intl_provider(base: &str) -> WorkbuddyProvider {
    WorkbuddyProvider::with_base(
        "workbuddy-intl",
        Region::Intl,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn chat_request(model: &str) -> ChatRequest {
    ChatRequest::new(
        model,
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::text("hello")],
        )],
    )
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn catalog() -> Value {
    json!({
        "code": 0,
        "data": {
            "models": [
                {
                    "id": "glm-5.2",
                    "name": "GLM 5.2",
                    "maxInputTokens": 200000,
                    "maxOutputTokens": 32000
                },
                {
                    "id": "hidden",
                    "name": "Hidden",
                    "maxInputTokens": 1000,
                    "maxOutputTokens": 100,
                    "disabled": true
                }
            ],
            "agents": [{"name": "cli", "models": ["glm-5.2", "hidden"]}]
        }
    })
}

fn content_stream() -> String {
    [
        frame(json!({"choices":[{"index":0,"delta":{"content":"Hel"}}]})),
        frame(json!({"choices":[{"index":0,"delta":{"content":"lo"}}]})),
        frame(json!({
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat()
}

fn assert_honest_headers(request: &Recorded) {
    assert_eq!(request.header("authorization"), Some("Bearer jwt-access"));
    assert_eq!(request.header("x-user-id"), Some("uid-1"));
    assert_eq!(request.header("x-enterprise-id"), Some("ent-1"));
    assert_eq!(request.header("x-tenant-id"), Some("ent-1"));
    assert_eq!(request.header("x-domain"), Some("www.codebuddy.cn"));
    assert!(request
        .header("user-agent")
        .is_some_and(|value| value.starts_with("polygate/")));
    for spoofed in [
        "x-device-token",
        "x-machine-id",
        "x-session-id",
        "x-codebuddy-request",
        "x-ide-version",
        "x-ide-name",
        "x-stainless-lang",
    ] {
        assert!(
            request.header(spoofed).is_none(),
            "adapter must not send `{spoofed}`"
        );
    }
}

#[tokio::test]
async fn model_discovery_uses_the_cn_enterprise_catalogue() {
    let server = spawn_server(|request| match request.path() {
        CN_MODELS => Reply::json(200, catalog().to_string()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let models = provider(&server.base_url)
        .list_models(&cn_credential())
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "glm-5.2");
    assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
    assert_eq!(models[0].context_window, Some(200000));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_honest_headers(&requests[0]);
}

#[tokio::test]
async fn intl_model_discovery_uses_v3_config_and_intl_domain() {
    let server = spawn_server(|request| match request.path() {
        INTL_CONFIG => Reply::json(200, catalog().to_string()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let models = intl_provider(&server.base_url)
        .list_models(&intl_credential())
        .await
        .unwrap();
    assert_eq!(models.len(), 1);

    let request = &server.requests()[0];
    assert_eq!(request.path(), INTL_CONFIG);
    assert_eq!(request.header("x-domain"), Some("www.workbuddy.ai"));
}

#[tokio::test]
async fn chat_aggregates_the_stream_into_one_response() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let mut request = chat_request("glm-5.2");
    request.max_output_tokens = Some(64);
    request.temperature = Some(0.2);
    let response = provider(&server.base_url)
        .chat(&cn_credential(), request)
        .await
        .unwrap();

    assert_eq!(response.model, "glm-5.2");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert_eq!(response.text(), "Hello");
    let usage = response.usage.expect("usage");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(usage.total(), 12);

    let sent = &server.requests()[0];
    assert_eq!(sent.method, "POST");
    assert_honest_headers(sent);
    let body: Value = serde_json::from_str(&sent.body).unwrap();
    assert_eq!(body["model"], "glm-5.2");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["messages"][0]["role"], "user");
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("enable_thinking").is_none());
}

#[tokio::test]
async fn tool_call_deltas_are_assembled() {
    let stream = [
        frame(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":""}}]}}]})),
        frame(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]})),
        frame(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Beijing\"}"}}]}}]})),
        frame(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    let server = spawn_server(move |_| Reply::sse(stream.clone())).await;

    let mut request = chat_request("glm-5.2");
    request.tools.push(crate::protocol::ToolDefinition::new(
        "lookup",
        json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    ));
    let response = provider(&server.base_url)
        .chat(&cn_credential(), request)
        .await
        .unwrap();

    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let calls: Vec<_> = response.message.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "lookup");
    assert_eq!(calls[0].arguments, "{\"city\":\"Beijing\"}");
}

#[tokio::test]
async fn tool_history_is_forwarded_without_reasoning() {
    let server = spawn_server(|_| Reply::sse(content_stream())).await;
    let assistant = ChatMessage::new(
        MessageRole::Assistant,
        vec![
            ContentBlock::reasoning("hidden reasoning"),
            ContentBlock::text("calling"),
            ContentBlock::ToolCall(crate::protocol::ToolCall::new(
                "call_1",
                "lookup",
                "{\"city\":\"Beijing\"}",
            )),
        ],
    );
    let mut request = ChatRequest::new(
        "glm-5.2",
        vec![
            ChatMessage::new(MessageRole::Developer, vec![ContentBlock::text("rules")]),
            assistant,
            ChatMessage::new(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult(ToolResult::new("call_1", "sunny"))],
            ),
        ],
    );
    request.max_output_tokens = None;
    provider(&server.base_url)
        .chat(&cn_credential(), request)
        .await
        .unwrap();

    let body: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "assistant");
    assert!(body["messages"][1].get("reasoning_content").is_none());
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
}

#[tokio::test]
async fn an_expired_credential_is_local_and_quota_is_not_blamed() {
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let expired = Credential::from_value(json!({
        "access_token": "jwt-access",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 1
    }));
    let status = provider(&server.base_url)
        .auth_status(&expired)
        .await
        .unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Expired);
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn auth_quota_and_restriction_errors_stay_distinct() {
    for (status, body, expected) in [
        (
            401u16,
            json!({"code": 401, "msg": "expired"}),
            ErrorKind::AuthExpired,
        ),
        (
            402u16,
            json!({"code": 14018, "msg": "credits exhausted"}),
            ErrorKind::QuotaExhausted,
        ),
        (
            403u16,
            json!({"code": 11140, "msg": "account restricted"}),
            ErrorKind::ChannelRestricted,
        ),
    ] {
        let server = spawn_server(move |_| Reply::json(status, body.to_string())).await;
        let error = provider(&server.base_url)
            .chat(&cn_credential(), chat_request("glm-5.2"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected, "status {status}");
    }
}

#[tokio::test]
async fn rate_limit_keeps_retry_after() {
    let server = spawn_server(|_| {
        Reply::json(
            429,
            json!({"code": 6020, "msg": "queued", "retryAfter": 2}).to_string(),
        )
    })
    .await;
    let error = provider(&server.base_url)
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .err()
        .expect("rate limit must fail the stream");
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(error.retry_after, Some(2));
}

#[tokio::test]
async fn upstream_5xx_is_classified() {
    let server = spawn_server(|_| Reply::json(503, "maintenance")).await;
    let error = provider(&server.base_url)
        .list_models(&cn_credential())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn request_timeout_is_reported() {
    let server = spawn_server(|_| Reply::slow(Duration::from_millis(300))).await;
    let provider =
        provider(&server.base_url).with_timeouts(Duration::from_millis(60), Duration::from_secs(1));
    let error = provider.list_models(&cn_credential()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn idle_stream_is_cut_off() {
    let first = frame(json!({"choices":[{"index":0,"delta":{"content":"a"}}]}));
    let second = frame(json!({"choices":[{"index":0,"delta":{"content":"b"}}]}));
    let split = first.len();
    let body = format!("{first}{second}");
    let server =
        spawn_server(move |_| Reply::sse_stalled(body.clone(), split, Duration::from_millis(300)))
            .await;
    let provider =
        provider(&server.base_url).with_timeouts(Duration::from_secs(1), Duration::from_millis(60));
    let mut stream = provider
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let mut saw_timeout = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                if let crate::protocol::StreamEvent::TextDelta { text } = event {
                    assert_eq!(text, "a");
                }
            }
            Err(error) => {
                assert_eq!(error.kind, ErrorKind::Timeout);
                saw_timeout = true;
                break;
            }
        }
    }
    assert!(saw_timeout, "idle stream must end with a timeout error");
}

#[tokio::test]
async fn a_truncated_stream_is_reported_as_an_error_event() {
    let server = spawn_server(|_| {
        Reply::sse(frame(
            json!({"choices":[{"index":0,"delta":{"content":"partial"}}]}),
        ))
    })
    .await;
    let mut stream = provider(&server.base_url)
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();
    let mut error = None;
    while let Some(item) = stream.next().await {
        let event = item.expect("stream items before the terminal event");
        if let crate::protocol::StreamEvent::Error { error: value } = event {
            error = Some(value);
        }
    }
    let error = error.expect("truncated stream must produce an error event");
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn a_json_error_envelope_on_200_is_not_a_stream() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"code": 11128, "msg": "Illegal API invocation from an unapproved channel"})
                .to_string(),
        )
    })
    .await;
    let error = provider(&server.base_url)
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .err()
        .expect("a JSON envelope must not be accepted as a stream");
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn dropping_the_stream_releases_the_request() {
    let server = spawn_server(|_| Reply::sse(content_stream())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();
    let first = stream.next().await.expect("start event").unwrap();
    assert!(matches!(first, crate::protocol::StreamEvent::Start { .. }));
    drop(stream);

    // The provider is still usable after a cancelled stream.
    let response = provider
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();
    assert_eq!(response.text(), "Hello");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn a_credential_from_another_region_is_rejected() {
    let server = spawn_server(|_| Reply::json(200, catalog().to_string())).await;
    let error = provider(&server.base_url)
        .list_models(&intl_credential())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn quota_reports_monthly_and_one_off_packages_in_credits() {
    let server = spawn_server(|request| match request.path() {
        "/v2/billing/meter/get-user-resource" => Reply::json(
            200,
            json!({"code": 0, "data": {"Response": {"Data": {"Accounts": [
                {
                    "Status": 0,
                    "CapacityType": 4,
                    "CycleCapacitySize": 100,
                    "CycleCapacityRemainPrecise": "40",
                    "CycleEndTime": "2999-01-01 00:00:00",
                    "PackageName": "pro monthly"
                },
                {
                    "Status": 0,
                    "CapacityType": 1,
                    "CapacitySize": "50",
                    "CapacityRemain": 20,
                    "ExpiredTime": 4_000_000_000_i64,
                    "PackageName": "one-off"
                }
            ]}}}})
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let snapshot = provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap();
    assert_eq!(snapshot.unit, crate::quota::QuotaUnit::Credits);
    assert_eq!(snapshot.status, crate::quota::QuotaStatus::Known);
    assert_eq!(snapshot.total, Some(150.0));
    assert_eq!(snapshot.remaining, Some(60.0));
    let request = &server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.header("authorization"), Some("Bearer jwt-access"));
    assert_honest_headers(request);
}

#[tokio::test]
async fn an_empty_quota_is_unknown_never_zero() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"code": 0, "data": {"Response": {"Data": {"Accounts": []}}}}).to_string(),
        )
    })
    .await;
    let snapshot = provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap();
    assert_eq!(snapshot.status, crate::quota::QuotaStatus::Unknown);
    assert_eq!(snapshot.total, None);
    assert_eq!(snapshot.remaining, None);
}

#[tokio::test]
async fn a_quota_failure_is_not_reported_as_zero_balance() {
    let server = spawn_server(|_| Reply::json(500, "{}")).await;
    let error = provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn capabilities_now_include_quota() {
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let capabilities = provider(&server.base_url).capabilities();
    assert!(capabilities.chat && capabilities.streaming && capabilities.tools);
    assert!(capabilities.quota);
    assert!(!capabilities.native_session && !capabilities.agent_task);
    assert_eq!(server.request_count(), 0);
}

// ---------------------------------------------------------------------------
// Credential refresh lifecycle
// ---------------------------------------------------------------------------

struct MemoryStore {
    current: Mutex<(Credential, i64)>,
    updates: Mutex<Vec<(String, i64)>>,
    conflict: AtomicBool,
    replacement: Option<Credential>,
    fail_update: AtomicBool,
    refreshes: Arc<AtomicUsize>,
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn update_credential(
        &self,
        account_id: &str,
        expected_revision: i64,
        credential: &Credential,
    ) -> Result<i64, GatewayError> {
        if self.fail_update.load(Ordering::SeqCst) {
            return Err(GatewayError::new(
                ErrorKind::Storage,
                "credential write failed",
            ));
        }
        if self.conflict.swap(false, Ordering::SeqCst) {
            let replacement = self
                .replacement
                .clone()
                .ok_or_else(|| GatewayError::internal("no replacement"))?;
            *self.current.lock() = (replacement, expected_revision + 1);
            return Err(GatewayError::bad_request(
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
    ) -> Result<Option<(Credential, i64)>, GatewayError> {
        Ok(Some(self.current.lock().clone()))
    }
}

fn store(current: Credential, revision: i64) -> Arc<MemoryStore> {
    Arc::new(MemoryStore {
        current: Mutex::new((current, revision)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(false),
        replacement: None,
        fail_update: AtomicBool::new(false),
        refreshes: Arc::new(AtomicUsize::new(0)),
    })
}

fn context() -> ProviderContext {
    ProviderContext {
        account_id: Some("account-1".into()),
        credential_revision: Some(7),
        ..ProviderContext::default()
    }
}

fn refreshable_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-old",
        "refresh_token": "refresh-old",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 1,
        "refresh_expires_at": 4_000_000_000_i64
    }))
}

fn refreshed_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-new",
        "refresh_token": "refresh-new",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 4_000_000_000_i64,
        "refresh_expires_at": 4_000_000_000_i64
    }))
}

fn refresh_handler(
    refreshes: Arc<AtomicUsize>,
) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    move |request: &Recorded| match request.path() {
        "/v2/plugin/auth/token/refresh" => {
            refreshes.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.header("x-refresh-token"), Some("refresh-old"));
            Reply::json(
                200,
                json!({"code": 0, "data": {
                    "accessToken": "jwt-new",
                    "refreshToken": "refresh-new",
                    "expires_in": 3600
                }})
                .to_string(),
            )
        }
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    }
}

fn provider_with_store(base: &str, store: Arc<MemoryStore>) -> WorkbuddyProvider {
    provider(base).with_credentials_store(store)
}

#[tokio::test]
async fn a_valid_credential_is_used_without_refreshing() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let valid = Credential::from_value(json!({
        "access_token": "jwt-valid",
        "refresh_token": "refresh-old",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 4_000_000_000_i64
    }));
    let response = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(&context(), &valid, chat_request("glm-5.2"))
        .await
        .unwrap();
    assert_eq!(response.text(), "Hello");
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 0);
    assert!(store.updates.lock().is_empty());
}

#[tokio::test]
async fn an_expired_credential_without_refresh_material_is_reported() {
    let store = store(
        Credential::from_value(json!({
            "access_token": "jwt-old",
            "uid": "uid-1",
            "domain": "www.codebuddy.cn",
            "expires_at": 1
        })),
        7,
    );
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let credential = store.current.lock().0.clone();
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(&context(), &credential, chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthExpired);
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn an_expired_refresh_token_is_auth_expired_without_a_request() {
    let expired_refresh = Credential::from_value(json!({
        "access_token": "jwt-old",
        "refresh_token": "refresh-old",
        "uid": "uid-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 1,
        "refresh_expires_at": 1
    }));
    let store = store(expired_refresh.clone(), 7);
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(&context(), &expired_refresh, chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthExpired);
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn a_rotated_refresh_token_is_persisted_before_the_chat() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let response = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap();
    assert_eq!(response.text(), "Hello");
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.updates.lock().as_slice(),
        &[("account-1".to_string(), 7)]
    );
    let (stored, revision) = store.current.lock().clone();
    assert_eq!(revision, 8);
    assert_eq!(stored.get("refresh_token"), Some("refresh-new"));
    assert_eq!(stored.get("access_token"), Some("jwt-new"));
    let chat = server
        .requests()
        .into_iter()
        .find(|request| request.path() == CHAT)
        .expect("chat request");
    assert_eq!(chat.header("authorization"), Some("Bearer jwt-new"));
}

#[tokio::test]
async fn concurrent_calls_refresh_once() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let shared = Arc::new(provider_with_store(&server.base_url, Arc::clone(&store)));
    let first = tokio::spawn({
        let provider = Arc::clone(&shared);
        async move {
            provider
                .chat_with_context(
                    &context(),
                    &refreshable_credential(),
                    chat_request("glm-5.2"),
                )
                .await
                .map(|response| response.text())
        }
    });
    let second = tokio::spawn({
        let provider = Arc::clone(&shared);
        async move {
            provider
                .chat_with_context(
                    &context(),
                    &refreshable_credential(),
                    chat_request("glm-5.2"),
                )
                .await
                .map(|response| response.text())
        }
    });
    assert_eq!(first.await.unwrap().unwrap(), "Hello");
    assert_eq!(second.await.unwrap().unwrap(), "Hello");
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(store.updates.lock().len(), 1);
}

#[tokio::test]
async fn a_cas_conflict_reloads_the_winners_credential() {
    let store = Arc::new(MemoryStore {
        current: Mutex::new((refreshable_credential(), 7)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(true),
        replacement: Some(refreshed_credential()),
        fail_update: AtomicBool::new(false),
        refreshes: Arc::new(AtomicUsize::new(0)),
    });
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let response = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap();
    assert_eq!(response.text(), "Hello");
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 1);
    assert!(store.updates.lock().is_empty(), "the loser must not write");
    let chat = server
        .requests()
        .into_iter()
        .find(|request| request.path() == CHAT)
        .expect("chat request");
    assert_eq!(chat.header("authorization"), Some("Bearer jwt-new"));
}

#[tokio::test]
async fn a_failed_persist_never_reports_a_successful_refresh() {
    let store = Arc::new(MemoryStore {
        current: Mutex::new((refreshable_credential(), 7)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(false),
        replacement: None,
        fail_update: AtomicBool::new(true),
        refreshes: Arc::new(AtomicUsize::new(0)),
    });
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 1);
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path() != CHAT),
        "no chat may run on a credential that was not persisted"
    );
}

#[tokio::test]
async fn auth_status_with_context_refreshes_and_reports_valid() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(refresh_handler(Arc::clone(&store.refreshes))).await;
    let status = provider_with_store(&server.base_url, Arc::clone(&store))
        .auth_status_with_context(&context(), &refreshable_credential())
        .await
        .unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Valid);
    assert_eq!(store.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(store.updates.lock().len(), 1);
}

/// A store whose refresh lease is held by "another process".
struct DenyingStore {
    current: Mutex<(Credential, i64)>,
}

#[async_trait]
impl CredentialStore for DenyingStore {
    async fn update_credential(
        &self,
        _account_id: &str,
        _expected_revision: i64,
        _credential: &Credential,
    ) -> Result<i64, GatewayError> {
        panic!("a denied lease must never persist a refresh");
    }

    async fn load_credential(
        &self,
        _account_id: &str,
    ) -> Result<Option<(Credential, i64)>, GatewayError> {
        Ok(Some(self.current.lock().clone()))
    }

    async fn acquire_refresh_lease(
        &self,
        _account_id: &str,
        _owner: &str,
        _now: i64,
        _ttl_seconds: i64,
    ) -> Result<bool, GatewayError> {
        Ok(false)
    }
}

#[tokio::test]
async fn another_process_holding_the_lease_is_waited_for_not_duplicated() {
    let store = Arc::new(DenyingStore {
        current: Mutex::new((refreshable_credential(), 7)),
    });
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>)
        .with_refresh_wait(2, Duration::from_millis(5));
    let error = provider
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(server.request_count(), 0, "no refresh and no chat may run");
}

#[tokio::test]
async fn a_rejected_refresh_is_auth_invalid_and_persists_nothing() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(|request| match request.path() {
        "/v2/plugin/auth/token/refresh" => Reply::json(
            401,
            json!({"code": 401, "msg": "refresh rejected"}).to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(store.updates.lock().is_empty());
    let (stored, revision) = store.current.lock().clone();
    assert_eq!(revision, 7);
    assert_eq!(stored.get("refresh_token"), Some("refresh-old"));
}

#[tokio::test]
async fn a_transport_failure_is_not_an_auth_failure() {
    let store = store(refreshable_credential(), 7);
    let server = spawn_server(|request| match request.path() {
        "/v2/plugin/auth/token/refresh" => Reply::json(503, "{}"),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
    assert_ne!(error.kind, ErrorKind::AuthInvalid);
    assert!(store.updates.lock().is_empty());
}

#[tokio::test]
async fn a_401_on_chat_triggers_one_forced_refresh_and_one_retry() {
    let store = store(refreshable_credential(), 7);
    let chats = Arc::new(AtomicUsize::new(0));
    let chat_counter = Arc::clone(&chats);
    let server = spawn_server(move |request: &Recorded| match request.path() {
        "/v2/plugin/auth/token/refresh" => Reply::json(
            200,
            json!({"code": 0, "data": {
                "accessToken": "jwt-new",
                "refreshToken": "refresh-new",
                "expires_in": 3600
            }})
            .to_string(),
        ),
        CHAT => {
            if chat_counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::json(401, json!({"code": 401, "msg": "expired"}).to_string())
            } else {
                Reply::sse(content_stream())
            }
        }
        _ => Reply::json(404, "{}"),
    })
    .await;
    let valid = Credential::from_value(json!({
        "access_token": "jwt-valid",
        "refresh_token": "refresh-old",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 4_000_000_000_i64,
        "refresh_expires_at": 4_000_000_000_i64
    }));
    let response = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(&context(), &valid, chat_request("glm-5.2"))
        .await
        .unwrap();
    assert_eq!(response.text(), "Hello");
    assert_eq!(chats.load(Ordering::SeqCst), 2);
    assert_eq!(store.updates.lock().len(), 1);
    let chat = server
        .requests()
        .into_iter()
        .filter(|request| request.path() == CHAT)
        .nth(1)
        .expect("retry");
    assert_eq!(chat.header("authorization"), Some("Bearer jwt-new"));
}

#[tokio::test]
async fn a_persistent_401_is_retried_only_once() {
    let store = store(refreshable_credential(), 7);
    let chats = Arc::new(AtomicUsize::new(0));
    let chat_counter = Arc::clone(&chats);
    let server = spawn_server(move |request: &Recorded| match request.path() {
        "/v2/plugin/auth/token/refresh" => Reply::json(
            200,
            json!({"code": 0, "data": {
                "accessToken": "jwt-new",
                "refreshToken": "refresh-new",
                "expires_in": 3600
            }})
            .to_string(),
        ),
        CHAT => {
            chat_counter.fetch_add(1, Ordering::SeqCst);
            Reply::json(401, json!({"code": 401, "msg": "expired"}).to_string())
        }
        _ => Reply::json(404, "{}"),
    })
    .await;
    let valid = Credential::from_value(json!({
        "access_token": "jwt-valid",
        "refresh_token": "refresh-old",
        "uid": "uid-1",
        "enterprise_id": "ent-1",
        "domain": "www.codebuddy.cn",
        "expires_at": 4_000_000_000_i64,
        "refresh_expires_at": 4_000_000_000_i64
    }));
    let error = provider_with_store(&server.base_url, Arc::clone(&store))
        .chat_with_context(&context(), &valid, chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthExpired);
    assert_eq!(chats.load(Ordering::SeqCst), 2, "exactly one retry");
}

// ---------------------------------------------------------------------------
// Client impersonation
// ---------------------------------------------------------------------------

fn impersonating_provider(base: &str) -> WorkbuddyProvider {
    provider(base).with_identity(crate::providers::profile::Identity::Impersonate)
}

#[tokio::test]
async fn impersonation_sends_the_desktop_identity_on_chat() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    impersonating_provider(&server.base_url)
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert_eq!(
        sent.header("user-agent"),
        Some("WorkBuddy/5.9.3 WorkBuddy/5.9.3 CLI/2.165.0")
    );
    assert_eq!(sent.header("x-codebuddy-request"), Some("1"));
    assert_eq!(sent.header("x-agent-intent"), Some("craft"));
    assert_eq!(sent.header("x-agent-runtime"), Some("local-agent"));
    assert_eq!(sent.header("x-stainless-runtime"), Some("node"));
    assert!(sent.header("x-conversation-id").is_some());
    assert_eq!(
        sent.header("x-conversation-request-id"),
        sent.header("x-root-request-id")
    );
    assert_eq!(
        sent.header("x-conversation-message-id"),
        sent.header("x-request-id")
    );
    // Account context is still the credential's, not the client's.
    assert_eq!(sent.header("x-user-id"), Some("uid-1"));
    assert_eq!(sent.header("x-domain"), Some("www.codebuddy.cn"));
}

#[tokio::test]
async fn impersonation_uses_the_language_variant_of_the_realm() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    intl_provider(&server.base_url)
        .with_identity(crate::providers::profile::Identity::Impersonate)
        .chat(&intl_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert_eq!(
        sent.header("user-agent"),
        Some("WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0")
    );
    // The Intl build names no agent runtime.
    assert_eq!(sent.header("x-agent-runtime"), None);
}

#[tokio::test]
async fn impersonation_keeps_chat_ids_off_quota_and_uses_the_desktop_ua() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"code": 0, "data": {"Response": {"Data": {"Accounts": []}}}}).to_string(),
        )
    })
    .await;

    impersonating_provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap();

    let request = &server.requests()[0];
    assert_eq!(request.header("user-agent"), Some("WorkBuddy/5.9.3"));
    assert_eq!(request.header("x-codebuddy-request"), Some("1"));
    assert_eq!(request.header("x-conversation-id"), None);
    assert_eq!(request.header("x-agent-intent"), None);
}

#[tokio::test]
async fn honest_mode_stays_free_of_client_headers() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    provider(&server.base_url)
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert!(sent.header("user-agent").unwrap().starts_with("polygate/"));
    assert_eq!(sent.header("x-codebuddy-request"), None);
    assert_eq!(sent.header("x-conversation-id"), None);
    assert_eq!(sent.header("x-stainless-runtime"), None);
}
