//! TRAE adapter tests. Every test talks to a local mock server; none reaches a
//! real upstream or holds a real credential.

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
    ChatMessage, ChatRequest, ContentBlock, FinishReason, MessageRole, StreamEvent, ToolResult,
};
use crate::providers::{Capability, CredentialStore, Provider, ProviderClass, ProviderContext};
use crate::quota::{QuotaStatus, QuotaUnit};
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::TraeProvider;

const CHAT: &str = "/api/agent/v3/llm_utils_chat";
const MODELS: &str = "/api/ide/v1/get_detail_param";
const USAGE: &str = "/trae/api/v2/pay/ide_user_ent_usage";
const PAY_STATUS: &str = "/trae/api/v1/pay/ide_user_pay_status";
const EXCHANGE: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";

fn cn_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-access",
        "uid": "uid-1",
        "domain": "trae.cn",
        "expires_at": 0
    }))
}

fn intl_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-intl",
        "uid": "uid-1",
        "domain": "trae.ai",
        "expires_at": 0
    }))
}

fn device_credential() -> Credential {
    Credential::from_value(json!({
        "accessToken": "jwt-access",
        "refreshToken": "refresh",
        "uid": "uid-1",
        "domain": "trae.cn",
        "machineId": "machine-1",
        "deviceId": "device-1"
    }))
}

fn expired_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-access",
        "uid": "uid-1",
        "domain": "trae.cn",
        "expires_at": 1
    }))
}

fn provider(base: &str) -> TraeProvider {
    TraeProvider::with_base(
        "trae-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn intl_provider(base: &str) -> TraeProvider {
    TraeProvider::with_base(
        "trae-intl",
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

fn frame(event: &str, value: Value) -> String {
    format!("event: {event}\ndata: {value}\n\n")
}

fn catalogue() -> Value {
    json!({
        "config_info_list": [
            {
                "config_name": "glm-5.2",
                "display_config": {"display_name": "GLM 5.2"},
                "context_window_tokens": {"dev": 200000, "max": 1000000}
            },
            {"config_name": "hidden"}
        ]
    })
}

fn content_stream() -> String {
    [
        frame("metadata", json!({"model": "glm-5.2"})),
        frame("output", json!({"response": "Hel"})),
        frame("output", json!({"response": "lo", "reasoning_content": "why"})),
        frame(
            "token_usage",
            json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12, "cache_read_input_tokens": 4}),
        ),
        frame("done", json!({"finish_reason": "stop"})),
    ]
    .concat()
}

fn chat_reply(body: String) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    move |_| Reply::sse(body.clone())
}

fn assert_honest_headers(request: &Recorded) {
    assert_eq!(
        request.header("authorization"),
        Some("Cloud-IDE-JWT jwt-access")
    );
    assert_eq!(request.header("x-cloudide-token"), Some("jwt-access"));
    assert_eq!(request.header("x-ide-token"), Some("jwt-access"));
    assert_eq!(request.header("x-uid"), Some("uid-1"));
    let ua = request.header("user-agent").unwrap_or_default();
    assert!(ua.starts_with("polygate/"), "unexpected UA {ua}");
    for absent in [
        "x-app-id",
        "x-app-version",
        "x-ide-version",
        "x-ide-version-code",
        "x-app-version-code",
        "x-ide-version-type",
        "x-device-type",
        "x-device-brand",
        "x-os-version",
        "request-traffic-type",
    ] {
        assert_eq!(request.header(absent), None, "unexpected header {absent}");
    }
}

#[tokio::test]
async fn non_streaming_chat_aggregates_named_events() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    let request = chat_request("glm-5.2");
    let response = provider.chat(&cn_credential(), request).await.unwrap();

    assert_eq!(response.model, "glm-5.2");
    assert!(matches!(response.finish_reason, FinishReason::Stop));
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
    let usage = response.usage.expect("usage");
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(usage.total_tokens, Some(12));
    assert_eq!(usage.cached_input_tokens, Some(4));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path(), CHAT);
    let body: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["function"], "solo_work_lite");
    assert_eq!(body["config_name"], "glm-5.2");
    assert_eq!(body["model"], "glm-5.2");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "hello"}])
    );
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn streaming_chat_emits_normalized_events() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut finish = None;
    let mut usage = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamEvent::Start { model, .. } => assert_eq!(model, "glm-5.2"),
            StreamEvent::TextDelta { text: part } => text.push_str(&part),
            StreamEvent::ReasoningDelta { text: part } => reasoning.push_str(&part),
            StreamEvent::Finish { reason } => finish = Some(reason),
            StreamEvent::Usage { usage: value } => usage = Some(value),
            _ => {}
        }
    }
    assert_eq!(text, "Hello");
    assert_eq!(reasoning, "why");
    assert!(matches!(finish, Some(FinishReason::Stop)));
    assert_eq!(usage.map(|usage| usage.input_tokens), Some(10));
}

#[tokio::test]
async fn honest_headers_never_impersonate_the_ide() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    provider
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let request = &server.requests()[0];
    assert_honest_headers(request);
    assert_eq!(request.header("x-machine-id"), None);
    assert_eq!(request.header("x-device-id"), None);
    assert_eq!(request.header("accept"), Some("text/event-stream"));
}

#[tokio::test]
async fn imported_device_identifiers_are_forwarded_not_invented() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    provider
        .chat(&device_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let request = &server.requests()[0];
    assert_eq!(request.header("x-machine-id"), Some("machine-1"));
    assert_eq!(request.header("x-device-id"), Some("device-1"));
}

#[tokio::test]
async fn models_use_the_directory_envelope() {
    let server = spawn_server(|request: &Recorded| {
        assert_eq!(request.path(), MODELS);
        Reply::json(200, catalogue().to_string())
    })
    .await;
    let provider = provider(&server.base_url);
    let models = provider.list_models(&cn_credential()).await.unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "glm-5.2");
    assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
    assert_eq!(models[0].context_window, Some(1_000_000));
    assert_eq!(models[1].id, "hidden");

    let request = &server.requests()[0];
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(body["function"], "solo_work_lite");
    assert_eq!(body["need_prompt"], false);
    assert_eq!(body["poly_prompt"], true);
    assert_eq!(request.header("accept"), Some("application/json"));
}

#[tokio::test]
async fn declared_capabilities_are_honest() {
    let provider = provider("http://127.0.0.1:9");
    let info = provider.info();
    assert_eq!(info.kind, crate::config::ProviderKind::Trae);
    assert_eq!(info.region, Region::Cn);
    assert_eq!(provider.capabilities().class(), Some(ProviderClass::Model));
    let capabilities = provider.capabilities();
    assert!(capabilities.chat);
    assert!(capabilities.streaming);
    assert!(capabilities.tools);
    assert!(capabilities.model_discovery);
    assert!(capabilities.quota);
    assert!(!capabilities.native_session);
    assert!(!capabilities.agent_task);
    assert!(!capabilities.cancellation);

    assert_eq!(
        crate::providers::unsupported_capability(&info, Capability::AgentTask).kind,
        ErrorKind::Unsupported
    );
}

#[tokio::test]
async fn expired_credentials_are_reported_and_not_sent() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);

    let status = provider.auth_status(&expired_credential()).await.unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Expired);
    let error = provider
        .chat(&expired_credential(), chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthExpired);
    assert_eq!(server.request_count(), 0);

    let status = provider.auth_status(&cn_credential()).await.unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Valid);
}

#[tokio::test]
async fn regions_never_share_credentials_or_hosts() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let intl = intl_provider(&server.base_url);
    let error = intl
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert_eq!(server.request_count(), 0);

    let response = intl
        .chat(&intl_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();
    assert!(!response.message.content.is_empty());
    assert_eq!(server.requests()[0].path(), CHAT);
}

#[tokio::test]
async fn upstream_failures_are_classified() {
    for (status, body, expected) in [
        (401, "{}", ErrorKind::AuthExpired),
        (
            402,
            r#"{"code":14018,"message":"credits"}"#,
            ErrorKind::QuotaExhausted,
        ),
        (
            200,
            r#"{"code":4008,"message":"quota"}"#,
            ErrorKind::QuotaExhausted,
        ),
        (
            400,
            r#"{"code":1005,"message":"plan"}"#,
            ErrorKind::ChannelRestricted,
        ),
        (429, r#"{"code":6004}"#, ErrorKind::ModelUnavailable),
        (400, "{}", ErrorKind::BadRequest),
    ] {
        let server = spawn_server(move |_| Reply::json(status, body)).await;
        let error = provider(&server.base_url)
            .chat(&cn_credential(), chat_request("glm-5.2"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected, "status {status} body {body}");
        assert_eq!(error.upstream_status, Some(status));
    }
}

#[tokio::test]
async fn an_in_stream_error_frame_becomes_a_stream_error() {
    let body = [
        frame("output", json!({"response": "partial"})),
        frame("error", json!({"code": 1005, "message": "plan required"})),
    ]
    .concat();
    let server = spawn_server(chat_reply(body)).await;
    let error = provider(&server.base_url)
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ChannelRestricted);
}

#[tokio::test]
async fn a_truncated_stream_is_reported_as_an_error_event() {
    let body = frame("output", json!({"response": "partial"}));
    let server = spawn_server(chat_reply(body)).await;
    let mut stream = provider(&server.base_url)
        .chat_stream(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let mut error = None;
    while let Some(item) = stream.next().await {
        let event = item.expect("items before the terminal error");
        if let StreamEvent::Error { error: value } = event {
            error = Some(value);
        }
    }
    let error = error.expect("truncated stream must produce an error event");
    assert_eq!(error.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn tool_calls_survive_streaming_fragments() {
    let body = [
        frame(
            "output",
            json!({"tool_calls": [{"index": 0, "id": "call-1", "function_call": {"name": "bash", "arguments": "{\"cmd\":"}}]}),
        ),
        frame(
            "output",
            json!({"tool_calls": [{"index": 0, "function_call": {"arguments": "\"ls\"}"}}]}),
        ),
        frame("done", json!({"finish_reason": "tool_calls"})),
    ]
    .concat();
    let server = spawn_server(chat_reply(body)).await;
    let response = provider(&server.base_url)
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    assert!(matches!(response.finish_reason, FinishReason::ToolCalls));
    let call = response
        .message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(call.id, "call-1");
    assert_eq!(call.name, "bash");
    assert_eq!(call.arguments, "{\"cmd\":\"ls\"}");
}

#[tokio::test]
async fn chat_tools_and_choice_reach_the_upstream() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    let mut request = chat_request("glm-5.2");
    request.tools.push(crate::protocol::ToolDefinition::new(
        "bash",
        json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
    ));
    request.tool_choice = Some(crate::protocol::ToolChoice::Tool("bash".into()));
    provider.chat(&cn_credential(), request).await.unwrap();

    let body: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
    assert_eq!(body["tool_choice"], "bash");
    assert!(body["tools"][0]["function"]["parameters"].is_string());
}

#[tokio::test]
async fn tool_history_is_converted_to_solo_shape() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);

    let mut assistant = ChatMessage::new(MessageRole::Assistant, Vec::new());
    assistant
        .content
        .push(ContentBlock::ToolCall(crate::protocol::ToolCall::new(
            "call-1", "bash", "{}",
        )));
    let request = ChatRequest::new(
        "glm-5.2",
        vec![
            ChatMessage::new(MessageRole::Developer, vec![ContentBlock::text("rules")]),
            assistant,
            ChatMessage::new(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult(ToolResult::new("call-1", "ok"))],
            ),
        ],
    );
    provider.chat(&cn_credential(), request).await.unwrap();

    let body: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], Value::Null);
    assert_eq!(
        body["messages"][1]["tool_calls"][0]["function_call"]["name"],
        "bash"
    );
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
}

#[tokio::test]
async fn an_orphan_tool_result_is_refused_before_any_request() {
    let server = spawn_server(chat_reply(content_stream())).await;
    let provider = provider(&server.base_url);
    let request = ChatRequest::new(
        "glm-5.2",
        vec![ChatMessage::new(
            MessageRole::Tool,
            vec![ContentBlock::ToolResult(ToolResult::new("missing", "ok"))],
        )],
    );
    let error = provider.chat(&cn_credential(), request).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest);
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn idle_stream_is_cut_off() {
    let first = frame("output", json!({"response": "a"}));
    let second = frame("output", json!({"response": "b"}));
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
            Ok(StreamEvent::TextDelta { text }) => assert_eq!(text, "a"),
            Ok(_) => {}
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
async fn a_slow_model_request_times_out() {
    let server = spawn_server(|_| Reply::slow(Duration::from_millis(300))).await;
    let provider =
        provider(&server.base_url).with_timeouts(Duration::from_millis(60), Duration::from_secs(1));
    let error = provider.list_models(&cn_credential()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
}

// ---------------------------------------------------------------------------
// Quota (M14-E)
// ---------------------------------------------------------------------------

fn credits_payload() -> Value {
    json!({
        "user_entitlement_pack_list": [
            {
                "group_name": "monthly credits",
                "entitlement_base_info": {
                    "quota": {"credits_limit": 100},
                    "end_time": 2_000_000_000_i64
                },
                "usage": {"credits_amount": 40}
            },
            {
                "group_name": "bonus credits",
                "entitlement_base_info": {
                    "quota": {"credits_limit": "20"},
                    "end_time": 1_900_000_000_i64
                },
                "usage": {"credits_amount": "5"}
            }
        ]
    })
}

#[tokio::test]
async fn cn_quota_sums_credit_packs_without_converting_units() {
    let server = spawn_server(|_| Reply::json(200, credits_payload().to_string())).await;
    let provider = provider(&server.base_url).with_ug_device_id(Some("1234567890123456".into()));
    let snapshot = provider.quota(&cn_credential()).await.unwrap();

    assert_eq!(snapshot.status, QuotaStatus::Known);
    assert_eq!(snapshot.unit, QuotaUnit::Credits);
    assert_eq!(snapshot.total, Some(120.0));
    assert_eq!(snapshot.remaining, Some(75.0));
    assert!(snapshot.error.is_none());

    let raw = snapshot.raw.expect("raw payload");
    assert_eq!(raw["period"], "per_package");
    assert_eq!(raw["realm"], "cn");
    assert_eq!(raw["packages"][0]["name"], "monthly credits");
    assert_eq!(raw["packages"][1]["remaining"], 15.0);
    assert_eq!(raw["expiry_ladder"].as_array().unwrap().len(), 2);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path(), USAGE);
    assert_eq!(requests[0].body, "{}");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Cloud-IDE-JWT jwt-access")
    );
    assert_eq!(requests[0].header("x-user-region"), Some("CN"));
    assert_eq!(requests[0].header("x-uid"), Some("uid-1"));
    assert_eq!(requests[0].header("x-device-id"), Some("1234567890123456"));
}

#[tokio::test]
async fn quota_without_a_pack_stays_unknown_never_zero() {
    let server =
        spawn_server(|_| Reply::json(200, json!({"user_entitlement_pack_list": []}).to_string()))
            .await;
    let snapshot = provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap();
    assert_eq!(snapshot.status, QuotaStatus::Unknown);
    assert_eq!(snapshot.unit, QuotaUnit::Credits);
    assert_eq!(snapshot.total, None);
    assert_eq!(snapshot.remaining, None);
    assert!(!snapshot.exhausted());
}

#[tokio::test]
async fn intl_quota_reports_the_plan_and_leaves_numbers_unknown() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({
                "user_pay_identity_str": "Pro",
                "trial_status": {"is_in_trial": true}
            })
            .to_string(),
        )
    })
    .await;
    let snapshot = intl_provider(&server.base_url)
        .quota(&intl_credential())
        .await
        .unwrap();
    assert_eq!(snapshot.status, QuotaStatus::Unknown);
    assert_eq!(snapshot.unit, QuotaUnit::Unknown);
    assert_eq!(snapshot.total, None);
    assert_eq!(snapshot.remaining, None);
    let raw = snapshot.raw.expect("raw payload");
    assert_eq!(raw["plan"], "Pro");
    assert_eq!(raw["in_trial"], true);
    assert_eq!(raw["period"], "subscription");
    assert_eq!(raw["realm"], "intl");

    let requests = server.requests();
    assert_eq!(requests[0].path(), PAY_STATUS);
    assert_eq!(requests[0].header("x-user-region"), Some("AI"));
}

// ---------------------------------------------------------------------------
// Refresh lifecycle (M14-E)
// ---------------------------------------------------------------------------

struct MemoryStore {
    current: Mutex<(Credential, i64)>,
    updates: Mutex<Vec<(String, i64)>>,
    conflict: AtomicBool,
    replacement: Mutex<Option<Credential>>,
    fail_update: AtomicBool,
    released: AtomicUsize,
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
                .lock()
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

    async fn release_refresh_lease(
        &self,
        _account_id: &str,
        _owner: &str,
    ) -> Result<(), GatewayError> {
        self.released.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn store(current: Credential, revision: i64) -> Arc<MemoryStore> {
    Arc::new(MemoryStore {
        current: Mutex::new((current, revision)),
        updates: Mutex::new(Vec::new()),
        conflict: AtomicBool::new(false),
        replacement: Mutex::new(None),
        fail_update: AtomicBool::new(false),
        released: AtomicUsize::new(0),
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
        "domain": "trae.cn",
        "expires_at": chrono::Utc::now().timestamp() - 60,
    }))
}

fn fresh_credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "jwt-fresh",
        "refresh_token": "refresh-fresh",
        "uid": "uid-1",
        "domain": "trae.cn",
        "expires_at": chrono::Utc::now().timestamp() + 3_600,
    }))
}

fn exchange_reply() -> Reply {
    Reply::json(
        200,
        json!({
            "Result": {
                "Token": "jwt-new",
                "RefreshToken": "refresh-new",
                "TokenExpireAt": chrono::Utc::now().timestamp_millis() + 86_400_000
            }
        })
        .to_string(),
    )
}

#[tokio::test]
async fn an_expiring_credential_is_refreshed_once_and_persisted_with_cas() {
    let server = spawn_server(|request| match request.path() {
        EXCHANGE => exchange_reply(),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let store = store(refreshable_credential(), 7);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);

    let status = provider
        .auth_status_with_context(&context(), &refreshable_credential())
        .await
        .unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Valid);

    assert_eq!(*store.updates.lock(), vec![("account-1".to_string(), 7)]);
    assert_eq!(store.released.load(Ordering::SeqCst), 1);
    let (current, revision) = store.current.lock().clone();
    assert_eq!(revision, 8);
    assert_eq!(current.get("access_token"), Some("jwt-new"));
    assert_eq!(current.get("refresh_token"), Some("refresh-new"));
    assert_eq!(current.get("machine_id"), Some(""));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path(), EXCHANGE);
    let body: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["RefreshToken"], "refresh-old");
    assert_eq!(body["ClientID"], "en1oxy7wnw8j9n");
}

#[tokio::test]
async fn a_fresh_credential_is_not_refreshed() {
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let store = store(fresh_credential(), 7);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);
    let status = provider
        .auth_status_with_context(&context(), &fresh_credential())
        .await
        .unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Valid);
    assert_eq!(server.request_count(), 0);
    assert!(store.updates.lock().is_empty());
}

#[tokio::test]
async fn a_rejected_refresh_marks_the_account_invalid() {
    let server = spawn_server(|_| Reply::json(401, r#"{"code":401}"#)).await;
    let store = store(refreshable_credential(), 7);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);
    let status = provider
        .auth_status_with_context(&context(), &refreshable_credential())
        .await
        .unwrap();
    assert_eq!(status, crate::accounts::AuthStatus::Invalid);
    assert_eq!(store.released.load(Ordering::SeqCst), 1);
    assert!(store.updates.lock().is_empty());
}

/// A store whose lease is held by another process.
struct LeasedStore {
    current: Mutex<(Credential, i64)>,
}

#[async_trait]
impl CredentialStore for LeasedStore {
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
    let store = Arc::new(LeasedStore {
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
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn a_cas_conflict_uses_the_winner_instead_of_refreshing_twice() {
    let server = spawn_server(|request| match request.path() {
        EXCHANGE => exchange_reply(),
        CHAT => Reply::sse(content_stream()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let winner = Credential::from_value(json!({
        "access_token": "jwt-winner",
        "refresh_token": "refresh-winner",
        "uid": "uid-1",
        "domain": "trae.cn",
        "expires_at": chrono::Utc::now().timestamp() + 86_400,
    }));
    let store = store(refreshable_credential(), 7);
    store.conflict.store(true, Ordering::SeqCst);
    *store.replacement.lock() = Some(winner);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);

    let response = provider
        .chat_with_context(
            &context(),
            &refreshable_credential(),
            chat_request("glm-5.2"),
        )
        .await
        .unwrap();
    assert_eq!(response.model, "glm-5.2");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one exchange and one chat");
    assert_eq!(requests[0].path(), EXCHANGE);
    assert_eq!(requests[1].path(), CHAT);
    assert_eq!(
        requests[1].header("authorization"),
        Some("Cloud-IDE-JWT jwt-winner")
    );
}

#[tokio::test]
async fn a_storage_failure_surfaces_instead_of_a_silent_retry() {
    let server = spawn_server(|request| match request.path() {
        EXCHANGE => exchange_reply(),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let store = store(refreshable_credential(), 7);
    store.fail_update.store(true, Ordering::SeqCst);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);
    let error = provider
        .auth_status_with_context(&context(), &refreshable_credential())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Storage);
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_401_on_chat_triggers_one_forced_refresh_and_retry() {
    let chats = Arc::new(AtomicUsize::new(0));
    let handler = {
        let chats = Arc::clone(&chats);
        move |request: &Recorded| match request.path() {
            CHAT => {
                if chats.fetch_add(1, Ordering::SeqCst) == 0 {
                    Reply::json(401, r#"{"code":401,"message":"token expired"}"#)
                } else {
                    Reply::sse(content_stream())
                }
            }
            EXCHANGE => exchange_reply(),
            _ => Reply::json(404, "{}"),
        }
    };
    let server = spawn_server(handler).await;
    // Valid for longer than the skew, but the upstream rejects it anyway.
    let store = store(fresh_credential(), 7);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);

    let response = provider
        .chat_with_context(&context(), &fresh_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();
    assert_eq!(response.model, "glm-5.2");
    assert_eq!(server.request_count(), 3, "chat, exchange, chat");
    let requests = server.requests();
    assert_eq!(requests[1].path(), EXCHANGE);
    assert_eq!(
        requests[2].header("authorization"),
        Some("Cloud-IDE-JWT jwt-new")
    );
    assert_eq!(*store.updates.lock(), vec![("account-1".to_string(), 7)]);
}

#[tokio::test]
async fn quota_with_context_refreshes_before_spending_the_call() {
    let server = spawn_server(|request| match request.path() {
        EXCHANGE => exchange_reply(),
        USAGE => Reply::json(200, credits_payload().to_string()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let store = store(refreshable_credential(), 7);
    let provider = provider(&server.base_url)
        .with_credentials_store(Arc::clone(&store) as Arc<dyn CredentialStore>);
    let snapshot = provider
        .quota_with_context(&context(), &refreshable_credential())
        .await
        .unwrap();
    assert_eq!(snapshot.status, QuotaStatus::Known);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path(), EXCHANGE);
    assert_eq!(requests[1].path(), USAGE);
    assert_eq!(
        requests[1].header("authorization"),
        Some("Cloud-IDE-JWT jwt-new")
    );
}

// ---------------------------------------------------------------------------
// Client impersonation
// ---------------------------------------------------------------------------

fn impersonating_provider(base: &str) -> TraeProvider {
    provider(base).with_identity(crate::providers::profile::Identity::Impersonate)
}

fn chat_reply_for(body: String) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    move |_| Reply::sse(body.clone())
}

#[tokio::test]
async fn impersonation_sends_the_ide_constants_on_chat() {
    let stream = content_stream();
    let server = spawn_server(chat_reply_for(stream)).await;

    impersonating_provider(&server.base_url)
        .chat(&cn_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert_eq!(sent.header("user-agent"), Some("Trae/0.1.52"));
    assert_eq!(
        sent.header("x-app-id"),
        Some("6eefa01c-1036-4c7e-9ca5-d891f63bfcd8")
    );
    assert_eq!(sent.header("x-ide-version"), Some("0.1.52"));
    assert_eq!(sent.header("x-ide-version-type"), Some("stable"));
    assert_eq!(sent.header("x-device-type"), Some("windows"));
    assert_eq!(sent.header("request-traffic-type"), Some("prod"));
    // Token headers stay as they were.
    assert_eq!(sent.header("x-cloudide-token"), Some("jwt-access"));
    assert_eq!(sent.header("x-ide-token"), Some("jwt-access"));
}

#[tokio::test]
async fn impersonation_keeps_imported_device_ids_and_omits_missing_ones() {
    let stream = content_stream();
    let server = spawn_server(chat_reply_for(stream)).await;

    impersonating_provider(&server.base_url)
        .chat(&device_credential(), chat_request("glm-5.2"))
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-machine-id"), Some("machine-1"));
    assert_eq!(sent.header("x-device-id"), Some("device-1"));
    assert_eq!(sent.header("user-agent"), Some("Trae/0.1.52"));
}

#[tokio::test]
async fn impersonation_uses_the_ide_ua_on_the_billing_endpoints_too() {
    let server = spawn_server(|_| Reply::json(200, credits_payload().to_string())).await;

    impersonating_provider(&server.base_url)
        .quota(&cn_credential())
        .await
        .unwrap();

    let sent = &server.requests()[0];
    assert_eq!(sent.header("user-agent"), Some("Trae/0.1.52"));
}
