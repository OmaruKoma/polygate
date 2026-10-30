//! OfficeAce adapter tests. Every test talks to a local mock server; none
//! reaches a real upstream or holds a real credential.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde_json::json;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::{ChatMessage, ChatRequest, FinishReason, StreamEvent};
use crate::providers::{CredentialStore, Provider, ProviderContext};
use crate::quota::{QuotaStatus, QuotaUnit};
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::OfficeAceProvider;

const MODELS: &str = "/v2/models";
const SUBSCRIPTION: &str = "/v1/subscription";
const TOKENS: &str = "/v1/oauth2/tokens";

fn dpop_jwk() -> serde_json::Value {
    let (_key, jwk) = crate::providers::codearts::auth::DpopKey::generate().expect("dpop key");
    jwk
}

fn chat_credential(base: &str) -> Credential {
    Credential::from_value(json!({
        "account_id": "account-1",
        "model_app_key": "app-key",
        "model_app_secret": "app-secret",
        "model_base_url": base,
    }))
}

fn control_plane_credential(base: &str) -> Credential {
    Credential::from_value(json!({
        "account_id": "account-1",
        "model_app_key": "app-key",
        "model_app_secret": "app-secret",
        "model_base_url": base,
        "access_key_id": "STS-AK",
        "secret_access_key": "STS-SK",
        "security_token": "STS-TOKEN",
        "project_id": "project-1",
        "expiration": "2030-01-01T00:00:00Z",
    }))
}

fn provider(endpoints: &str) -> OfficeAceProvider {
    OfficeAceProvider::with_endpoints(
        "officeace-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        endpoints,
        endpoints,
    )
}

fn catalogue() -> String {
    json!({
        "data": [
            {"id": "glm-5.2", "name": "GLM 5.2", "context_length": 128000},
            {"id": "qwen3-max"}
        ]
    })
    .to_string()
}

fn chat_request() -> ChatRequest {
    ChatRequest::new("glm-5.2", vec![ChatMessage::user("hello")])
}

fn stream_body() -> String {
    [
        json!({"id": "resp-1", "model": "glm-5.2",
               "choices": [{"delta": {"reasoning_content": "think"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {"content": "hello"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 2,
                                        "total_tokens": 5}}),
    ]
    .into_iter()
    .map(|value| format!("data: {value}\n\n"))
    .collect::<String>()
        + "data: [DONE]\n\n"
}

#[tokio::test]
async fn models_use_basic_auth_and_parse_the_catalogue() {
    let server = spawn_server(|request| match request.path() {
        MODELS => Reply::json(200, catalogue()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let provider = provider(&server.base_url);
    let models = provider
        .list_models(&chat_credential(&server.base_url))
        .await
        .expect("catalogue");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "glm-5.2");

    let sent: &Recorded = &server.requests()[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(
        sent.header("authorization"),
        Some("Basic YXBwLWtleTphcHAtc2VjcmV0")
    );
}

#[tokio::test]
async fn a_stream_becomes_reasoning_text_usage_and_finish() {
    let body = stream_body();
    let server = spawn_server(move |_| Reply::sse(body.clone())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&chat_credential(&server.base_url), chat_request())
        .await
        .expect("stream");
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("event"));
    }
    assert!(matches!(events[0], StreamEvent::Start { .. }));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::ReasoningDelta { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "hello")));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::Usage { .. })));
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Finish {
            reason: FinishReason::Stop
        })
    );

    let sent = &server.requests()[0];
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
    let body: serde_json::Value = serde_json::from_str(&sent.body).expect("body");
    assert_eq!(body["stream"], json!(true));
}

#[tokio::test]
async fn a_non_streaming_call_aggregates_the_stream() {
    let body = stream_body();
    let server = spawn_server(move |_| Reply::sse(body.clone())).await;
    let provider = provider(&server.base_url);
    let response = provider
        .chat(&chat_credential(&server.base_url), chat_request())
        .await
        .expect("chat");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert_eq!(response.message.text_content(), "hello");
    assert_eq!(response.usage.expect("usage").total(), 5);
}

#[tokio::test]
async fn a_fake_200_error_frame_is_a_typed_error() {
    let body = concat!(
        "data: {\"choices\": [{\"delta\": {\"content\": \"a\"}}]}\n\n",
        "data: {\"error\": {\"code\": \"ModelArts.81004\", \"message\": \"no permission\"}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let server = spawn_server(move |_| Reply::sse(body.clone())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&chat_credential(&server.base_url), chat_request())
        .await
        .expect("stream");
    let mut error = None;
    while let Some(item) = stream.next().await {
        if let StreamEvent::Error { error: reported } = item.expect("event") {
            error = Some(reported);
        }
    }
    let error = error.expect("error event");
    assert_eq!(error.kind, ErrorKind::ModelUnavailable);
}

#[tokio::test]
async fn quota_reads_points_with_a_v11_signature() {
    let subscription = json!({
        "skus": [{"quotas": [{
            "sku_attr_code": "officeace_points", "sku_value": "4000",
            "current_value": "1000", "expired_time": "2030-01-01T00:00:00Z"
        }]}],
        "bonus_skus": [{
            "activity_id": "bonus-daily", "activity_name": "每日签到",
            "cbc_resource_id": "bonus:daily", "points": "1000",
            "current_value": "0", "expired_time": "2030-01-01T00:00:00Z"
        }]
    })
    .to_string();
    let server = spawn_server(move |request| match request.path() {
        SUBSCRIPTION => Reply::json(200, subscription.clone()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let provider = provider(&server.base_url);
    let context = ProviderContext {
        account_id: Some("account-1".into()),
        credential_revision: Some(0),
        ..ProviderContext::default()
    };
    let snapshot = provider
        .quota_with_context(&context, &control_plane_credential(&server.base_url))
        .await
        .expect("quota");
    assert_eq!(snapshot.unit, QuotaUnit::Credits);
    assert_eq!(snapshot.remaining, Some(4000.0));
    assert_eq!(snapshot.status, QuotaStatus::Known);

    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-subscription-type"), Some("v2"));
    let auth = sent.header("authorization").unwrap_or_default();
    assert!(auth.starts_with("V11-HMAC-SHA256 Credential=STS-AK/"));
    assert!(auth.contains("x-subscription-type"));
}

#[tokio::test]
async fn a_chat_only_credential_reports_unknown_quota_without_numbers() {
    let server = spawn_server(|_| Reply::json(200, "{}")).await;
    let provider = provider(&server.base_url);
    let context = ProviderContext {
        account_id: Some("account-1".into()),
        ..ProviderContext::default()
    };
    let snapshot = provider
        .quota_with_context(&context, &chat_credential(&server.base_url))
        .await
        .expect("quota");
    assert_eq!(snapshot.status, QuotaStatus::Unknown);
    assert_eq!(snapshot.remaining, None);
    assert_eq!(snapshot.total, None);
    assert!(server.requests().is_empty(), "no upstream call is possible");
}

#[tokio::test]
async fn a_rejected_app_key_is_reported_as_invalid() {
    let server = spawn_server(|_| {
        Reply::json(
            401,
            r#"{"error":"APIG.1009 AppKey or AppSecret is invalid"}"#,
        )
    })
    .await;
    let provider = provider(&server.base_url);
    let status = provider
        .auth_status(&chat_credential(&server.base_url))
        .await
        .expect("status");
    assert_eq!(status, AuthStatus::Invalid);
}

#[tokio::test]
async fn the_login_flow_exchanges_the_code_before_it_needs_the_chat_key() {
    use super::http::OfficeAceHttp;
    use super::login::{self, LoginConfig, LoginSecrets};

    let server = spawn_server(|request| match request.path() {
        TOKENS => Reply::json(
            200,
            json!({
                "credentials": {
                    "access_key_id": "NEW-AK",
                    "secret_access_key": "NEW-SK",
                    "security_token": "NEW-TOKEN",
                    "project_id": "project-1",
                    "expiration": "2030-01-01T00:00:00Z"
                },
                "refresh_token": "refresh-new"
            })
            .to_string(),
        ),
        "/v1/claw/client-permission-validate" => Reply::json(
            200,
            json!({
                "account_id": "acc-1",
                "principal_urn": "iam::1:user:alice",
                "subscription": {"model_info": {
                    "model_api_url_base": "https://gw.example/v2",
                    "model_auth_info": {"model_app_key": "app-key", "model_app_secret": "app-secret"}
                }}
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let http =
        OfficeAceHttp::with_endpoints(reqwest::Client::new(), &server.base_url, &server.base_url);
    let config = LoginConfig {
        cloud: server.base_url.clone(),
        sts: server.base_url.clone(),
        ..LoginConfig::default()
    };
    let (_key, jwk) = crate::providers::codearts::auth::DpopKey::generate().expect("dpop key");
    let secrets = LoginSecrets {
        state: "state-1".into(),
        code_verifier: "verifier-1".into(),
        dpop_private_jwk: jwk,
    };
    let outcome = login::complete(&http, &config, "code-1", &secrets)
        .await
        .expect("login completes before the chat key is known");
    assert_eq!(outcome.account_id, "acc-1");
    assert_eq!(outcome.credential["model_app_key"], json!("app-key"));
    assert_eq!(
        outcome.credential["model_base_url"],
        json!("https://gw.example")
    );

    let requests = server.requests();
    assert_eq!(requests[0].path(), TOKENS);
    assert!(requests[0].header("DPoP").is_some());
    assert!(requests[0].body.contains("grant_type=authorization_code"));
    assert_eq!(requests[1].path(), "/v1/claw/client-permission-validate");
    assert!(requests[1]
        .header("authorization")
        .unwrap_or_default()
        .starts_with("SDK-HMAC-SHA256 Access=NEW-AK,"));
}

struct MemoryStore {
    current: Mutex<(Credential, i64)>,
    updates: Mutex<Vec<i64>>,
    token_calls: AtomicUsize,
}

#[async_trait]
impl CredentialStore for MemoryStore {
    async fn update_credential(
        &self,
        _account_id: &str,
        expected_revision: i64,
        credential: &Credential,
    ) -> Result<i64, crate::error::GatewayError> {
        self.updates.lock().push(expected_revision);
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

#[tokio::test]
async fn an_expiring_token_is_refreshed_once_and_persisted() {
    let store = Arc::new(MemoryStore {
        current: Mutex::new((Credential::from_value(json!({})), 7)),
        updates: Mutex::new(Vec::new()),
        token_calls: AtomicUsize::new(0),
    });
    let jwk = dpop_jwk();
    let expiration = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();

    let counter = Arc::clone(&store);
    let server = spawn_server(move |request| match request.path() {
        TOKENS => {
            counter.token_calls.fetch_add(1, Ordering::SeqCst);
            assert!(request.header("DPoP").is_some(), "DPoP proof is required");
            assert!(request.body.contains("grant_type=refresh_token"));
            assert!(request.body.contains("client_id=pdp5_for_agentarts"));
            Reply::json(
                200,
                json!({
                    "credentials": {
                        "access_key_id": "STS-AK-2",
                        "secret_access_key": "STS-SK-2",
                        "security_token": "STS-TOKEN-2",
                        "project_id": "project-1",
                        "expiration": "2030-01-01T00:00:00Z"
                    },
                    "refresh_token": "refresh-2"
                })
                .to_string(),
            )
        }
        MODELS => Reply::json(200, catalogue()),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let credential = Credential::from_value(json!({
        "account_id": "account-1",
        "model_app_key": "app-key",
        "model_app_secret": "app-secret",
        "model_base_url": server.base_url,
        "access_key_id": "STS-AK",
        "secret_access_key": "STS-SK",
        "security_token": "STS-TOKEN",
        "project_id": "project-1",
        "expiration": expiration,
        "refresh_token": "refresh-1",
        "client_id": "pdp5_for_agentarts",
        "dpop_private_jwk": jwk,
    }));
    *store.current.lock() = (credential.clone(), 7);

    let provider = provider(&server.base_url).with_credentials_store(store.clone());
    let context = ProviderContext {
        account_id: Some("account-1".into()),
        credential_revision: Some(7),
        ..ProviderContext::default()
    };
    let status = provider
        .auth_status_with_context(&context, &credential)
        .await
        .expect("status");
    assert_eq!(status, AuthStatus::Valid);
    assert_eq!(store.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.updates.lock().as_slice(), &[7]);

    let (stored, revision) = store.current.lock().clone();
    assert_eq!(revision, 8);
    assert_eq!(stored.to_json()["refresh_token"], json!("refresh-2"));
    assert_eq!(stored.to_json()["access_key_id"], json!("STS-AK-2"));

    let models = server
        .requests()
        .into_iter()
        .find(|request| request.path() == MODELS)
        .expect("model probe");
    assert!(models
        .header("authorization")
        .unwrap_or_default()
        .starts_with("Basic "));
}
