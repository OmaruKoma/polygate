//! TeleAgent adapter tests. Every test talks to a local mock server; none
//! reaches a real upstream or holds a real credential.
//!
//! The signature is re-derived here from the headers the client sent, using the
//! documented algorithm, so a wiring mistake (wrong URI, unsigned body bytes,
//! stale timestamp) fails the test even though the golden vector lives in
//! `signer`.

use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::{
    ChatMessage, ChatRequest, ContentBlock, FinishReason, MessageRole, StreamEvent,
};
use crate::providers::{Provider, ProviderContext};
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::TeleagentProvider;

const MODEL_OPTIONS: &str = "/superCowork/sapi/api/v1/model-options";
const CHAT: &str = "/superCowork/sapi/api/v1/chat/completions";
const USER_CENTER: &str = "/superCowork/sapi/user/portal/companyStore/userCenter";

fn credential() -> Credential {
    Credential::from_value(json!({
        "token": "Em9xR4:header.payload.signature-part",
        "device_id": "dev-1",
        "install_id": "install-1",
    }))
}

fn provider(base: &str) -> TeleagentProvider {
    TeleagentProvider::with_base(
        "teleagent-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn catalogue() -> Value {
    json!({
        "code": 0,
        "message": "ok",
        "data": {
            "language": "zh-CN",
            "models": [
                {"model": "chat-pro", "display_name": "智能", "is_default": true,
                 "context_limit": null, "points_multiplier": "0.6"},
                {"model": "chat-flash", "display_name": "极速", "context_limit": null,
                 "points_multiplier": "0.2"}
            ]
        }
    })
}

fn chat_request() -> ChatRequest {
    ChatRequest::new(
        "chat-pro",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::text("hello")],
        )],
    )
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn hmac_hex(key: &str, message: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("key");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Recompute the expected `X-Superagent-Signature` from what the client sent.
fn expected_signature(recorded: &Recorded) -> String {
    let token = recorded.header("x-token").expect("x-token").to_string();
    let install = recorded
        .header("x-superagent-install-id")
        .expect("install id")
        .to_string();
    let timestamp = recorded
        .header("x-superagent-timestamp")
        .expect("timestamp")
        .to_string();
    let nonce = recorded
        .header("x-superagent-nonce")
        .expect("nonce")
        .to_string();
    let app = recorded.header("x-app-version").expect("app version");
    let jwt3 = token.rsplit('.').next().unwrap_or(token.as_str());
    let step = hmac_hex(
        jwt3,
        &[
            "superagent-auth-v1",
            token.as_str(),
            install.as_str(),
            timestamp.as_str(),
            nonce.as_str(),
        ]
        .join("/"),
    );
    let body_hash = hex::encode(Sha256::digest(recorded.body.as_bytes()));
    hmac_hex(
        &step,
        &[
            "superagent-auth-v1",
            recorded.method.as_str(),
            recorded.target.as_str(),
            timestamp.as_str(),
            nonce.as_str(),
            app,
            &body_hash,
        ]
        .join("\n"),
    )
}

#[tokio::test]
async fn every_model_call_carries_the_identity_headers() {
    let server = spawn_server(|request| match request.path() {
        MODEL_OPTIONS => Reply::json(200, catalogue().to_string()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    provider(&server.base_url)
        .list_models(&credential())
        .await
        .expect("catalogue");

    let sent: &Recorded = &server.requests()[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer sk-qvp4m6h9g20uM6PgYP7E")
    );
    assert_eq!(
        sent.header("x-token"),
        Some("Em9xR4:header.payload.signature-part")
    );
    assert!(sent.header("x-nonce").is_some());
    assert!(sent.header("x-timestamp").is_some());
    assert_eq!(sent.header("x-route-target"), Some("ops-gateway"));
    assert_eq!(
        sent.header("x-app-version"),
        Some(super::region::DEFAULT_APP_VERSION)
    );
}

#[tokio::test]
async fn a_chat_call_is_signed_over_the_exact_body_bytes() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::json(
            200,
            json!({
                "id": "resp-1",
                "model": "chat-pro",
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "pong"}
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;
    provider(&server.base_url)
        .chat(&credential(), chat_request())
        .await
        .expect("chat");

    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-superagent-sign-version"), Some("v1"));
    assert_eq!(sent.header("x-superagent-device-id"), Some("dev-1"));
    assert_eq!(sent.header("x-superagent-install-id"), Some("install-1"));
    let signature = sent.header("x-superagent-signature").expect("signature");
    assert_eq!(signature.len(), 64, "hex sha256");
    assert_eq!(signature, expected_signature(sent));
}

#[tokio::test]
async fn a_tampered_body_would_not_match_the_signature() {
    // Guard the signer against "hash the parsed JSON instead of the bytes":
    // the recorded body is what was signed, so flipping one byte must not
    // reproduce the header.
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"choices": [{"finish_reason": "stop", "message": {"content": "ok"}}]})
                .to_string(),
        )
    })
    .await;
    provider(&server.base_url)
        .chat(&credential(), chat_request())
        .await
        .expect("chat");
    let sent = &server.requests()[0];
    let mut altered = Recorded {
        method: sent.method.clone(),
        target: sent.target.clone(),
        headers: sent.headers.clone(),
        body: format!("{} ", sent.body),
    };
    let real = sent.header("x-superagent-signature").unwrap().to_string();
    altered.body = sent.body.clone();
    assert_eq!(expected_signature(&altered), real);
    altered.body.push(' ');
    assert_ne!(expected_signature(&altered), real);
}

#[tokio::test]
async fn the_catalogue_is_exposed_verbatim() {
    let server = spawn_server(|_| Reply::json(200, catalogue().to_string())).await;
    let models = provider(&server.base_url)
        .list_models(&credential())
        .await
        .expect("catalogue");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "chat-pro");
    assert_eq!(models[0].context_window, Some(400_000));
}

#[tokio::test]
async fn a_rejected_token_probes_as_expired() {
    let server =
        spawn_server(|_| Reply::json(401, r#"{"code":40108,"message":"invalid token"}"#)).await;
    let status = provider(&server.base_url)
        .auth_status(&credential())
        .await
        .expect("probe");
    assert_eq!(status, AuthStatus::Expired);
}

#[tokio::test]
async fn a_healthy_token_probes_as_valid() {
    let server = spawn_server(|_| Reply::json(200, catalogue().to_string())).await;
    let status = provider(&server.base_url)
        .auth_status(&credential())
        .await
        .expect("probe");
    assert_eq!(status, AuthStatus::Valid);
}

#[tokio::test]
async fn a_non_streaming_chat_keeps_reasoning_and_text() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({
                "id": "resp-1",
                "model": "chat-pro",
                "choices": [{
                    "finish_reason": "stop",
                    "message": {
                        "role": "assistant",
                        "content": "pong",
                        "reasoning_content": "the user said hello"
                    }
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7,
                          "reasoning_tokens": 2}
            })
            .to_string(),
        )
    })
    .await;
    let response = provider(&server.base_url)
        .chat(&credential(), chat_request())
        .await
        .expect("chat");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    let reasoning: String = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "the user said hello");
    assert_eq!(response.usage.expect("usage").input_tokens, 5);
}

#[tokio::test]
async fn a_streamed_chat_yields_deltas_and_a_finish() {
    let body = [
        frame(json!({
            "id": "resp-1", "model": "chat-pro",
            "choices": [{"delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
        })),
        frame(json!({
            "choices": [{"delta": {"reasoning_content": "嗯"}, "finish_reason": null}]
        })),
        frame(json!({
            "choices": [{"delta": {"content": "po"}, "finish_reason": null}]
        })),
        frame(json!({
            "choices": [{"delta": {"content": "ng"}, "finish_reason": "stop"}]
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();
    let server = spawn_server(move |_| Reply::sse(body.clone())).await;

    let mut stream = provider(&server.base_url)
        .chat_stream(&credential(), chat_request())
        .await
        .expect("stream");
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        match item.expect("event") {
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(&delta),
            StreamEvent::Finish { reason } => finish = Some(reason),
            _ => {}
        }
    }
    assert_eq!(text, "pong");
    assert_eq!(reasoning, "嗯");
    assert_eq!(finish, Some(FinishReason::Stop));
    let sent = &server.requests()[0];
    assert_eq!(sent.header("x-superagent-sign-version"), Some("v1"));
    assert_eq!(
        sent.header("x-superagent-signature"),
        Some(expected_signature(sent).as_str())
    );
}

#[tokio::test]
async fn the_points_balance_becomes_a_credits_snapshot() {
    let server = spawn_server(|request| match request.path() {
        USER_CENTER => Reply::json(
            200,
            json!({
                "code": 200,
                "msg": "success",
                "data": {"current_points_balance": "1234.50", "plan_display_name": "免费版"}
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let context = ProviderContext {
        account_id: Some("acc-1".into()),
        ..Default::default()
    };
    let snapshot = provider(&server.base_url)
        .quota_with_context(&context, &credential())
        .await
        .expect("quota");
    assert_eq!(snapshot.account_id, "acc-1");
    assert_eq!(snapshot.remaining, Some(1234.5));
    assert_eq!(snapshot.total, None);
}

#[tokio::test]
async fn a_json_error_returned_with_200_never_becomes_a_stream() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"code": 40108, "message": "invalid token"}).to_string(),
        )
    })
    .await;
    match provider(&server.base_url)
        .chat_stream(&credential(), chat_request())
        .await
    {
        Ok(_) => panic!("a JSON error body must not become a stream"),
        Err(error) => assert_eq!(error.kind, ErrorKind::AuthExpired),
    }
}
