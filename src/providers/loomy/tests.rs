//! Loomy adapter tests. Every test talks to a local mock server; none reaches
//! a real upstream or holds a real credential.

use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::{
    ChatMessage, ChatRequest, ContentBlock, FinishReason, MessageRole, StreamEvent,
};
use crate::providers::{Provider, ProviderContext};
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::LoomyProvider;

const MODELS: &str = "/models";
const CHAT: &str = "/chat/completions";
const POINTS: &str = "/points/records";

fn credential() -> Credential {
    Credential::from_value(json!({
        "session": "0123456789abcdef0123456789abcdef",
        "userid": "u-1",
        "expires_at": 0
    }))
}

fn provider(base: &str) -> LoomyProvider {
    LoomyProvider::with_base(
        "loomy-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn catalogue() -> Value {
    json!({
        "object": "list",
        "data": [
            {"id": "deepseek-v4-flash-0731", "name": "DeepSeek V4 Flash (x0.3)",
             "context_length": 262144, "max_output_tokens": 32768},
            {"id": "spark-x", "name": "Spark X (x0.1)"}
        ]
    })
}

fn chat_request() -> ChatRequest {
    ChatRequest::new(
        "deepseek-v4-flash-0731",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::text("hello")],
        )],
    )
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

#[tokio::test]
async fn every_model_call_carries_the_documented_headers() {
    let server = spawn_server(|request| match request.path() {
        MODELS => Reply::json(200, catalogue().to_string()),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let provider = provider(&server.base_url);
    provider
        .list_models(&credential())
        .await
        .expect("catalogue");

    let sent: &Recorded = &server.requests()[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(
        sent.header("authorization"),
        Some("Bearer 0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        sent.header("token"),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        sent.header("loomy-version"),
        Some(super::region::LOOMY_VERSION)
    );
    let traceparent = sent.header("traceparent").expect("traceparent");
    assert!(traceparent.starts_with("00-"), "{traceparent}");
}

#[tokio::test]
async fn the_catalogue_is_exposed_verbatim() {
    let server = spawn_server(|_| Reply::json(200, catalogue().to_string())).await;
    let models = provider(&server.base_url)
        .list_models(&credential())
        .await
        .expect("catalogue");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "deepseek-v4-flash-0731");
    assert_eq!(models[0].context_window, Some(262144));
}

#[tokio::test]
async fn a_rejected_session_maps_to_expired_not_a_hard_error() {
    let server = spawn_server(|_| Reply::json(401, r#"{"message":"session expired"}"#)).await;
    let status = provider(&server.base_url)
        .auth_status(&credential())
        .await
        .expect("probe");
    assert_eq!(status, AuthStatus::Expired);
}

#[tokio::test]
async fn a_healthy_session_probes_as_valid() {
    let server = spawn_server(|_| Reply::json(200, catalogue().to_string())).await;
    let status = provider(&server.base_url)
        .auth_status(&credential())
        .await
        .expect("probe");
    assert_eq!(status, AuthStatus::Valid);
}

#[tokio::test]
async fn a_non_streaming_chat_returns_text_and_usage() {
    let server = spawn_server(|request| match request.path() {
        CHAT => Reply::json(
            200,
            json!({
                "id": "resp-1",
                "model": "deepseek-v4-flash-0731",
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "pong"}
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7,
                          "points_consumed": 3}
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;
    let response = provider(&server.base_url)
        .chat(&credential(), chat_request())
        .await
        .expect("chat");
    assert_eq!(response.finish_reason, FinishReason::Stop);
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "pong");
    assert_eq!(response.usage.expect("usage").input_tokens, 5);

    let sent = &server.requests()[0];
    let body: Value = serde_json::from_str(&sent.body).expect("json body");
    assert_eq!(body["stream"], false);
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["messages"][0]["content"], "hello");
}

#[tokio::test]
async fn a_streamed_chat_yields_deltas_and_a_finish() {
    let body = [
        frame(json!({
            "id": "resp-1", "model": "deepseek-v4-flash-0731",
            "choices": [{"delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
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
    let mut finish = None;
    while let Some(item) = stream.next().await {
        match item.expect("event") {
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::Finish { reason } => finish = Some(reason),
            _ => {}
        }
    }
    assert_eq!(text, "pong");
    assert_eq!(finish, Some(FinishReason::Stop));

    let sent = &server.requests()[0];
    let body: Value = serde_json::from_str(&sent.body).expect("json body");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn points_become_a_credits_snapshot() {
    let server = spawn_server(|request| match request.path() {
        POINTS => Reply::json(
            200,
            json!({"data": {"balance": 5500, "dailyBalance": 4976, "availableBalance": 10476}})
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
    assert_eq!(snapshot.remaining, Some(10476.0));
    assert_eq!(snapshot.total, Some(10476.0));
}

#[tokio::test]
async fn exhausted_points_are_classified_before_the_body_is_parsed() {
    let server = spawn_server(|_| Reply::json(402, r#"{"message":"no points"}"#)).await;
    let error = provider(&server.base_url)
        .quota_with_context(&ProviderContext::default(), &credential())
        .await
        .expect_err("exhausted");
    assert_eq!(error.kind, ErrorKind::QuotaExhausted);
}

#[tokio::test]
async fn a_json_error_returned_with_200_never_becomes_a_stream() {
    let server = spawn_server(|_| {
        Reply::json(
            200,
            json!({"code": 401, "message": "token invalid"}).to_string(),
        )
    })
    .await;
    match provider(&server.base_url)
        .chat_stream(&credential(), chat_request())
        .await
    {
        Ok(_) => panic!("a JSON error body must not become a stream"),
        Err(error) => assert_eq!(error.kind, ErrorKind::AuthInvalid),
    }
}
