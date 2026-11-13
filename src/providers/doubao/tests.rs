//! Doubao adapter tests. Every test talks to a local mock server; none reaches
//! a real upstream or holds a real credential.

use futures_util::StreamExt;
use serde_json::json;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::{ChatMessage, ChatRequest, ContentBlock, MessageRole, StreamEvent};
use crate::providers::Provider;
use crate::test_support::{spawn_server, Recorded, Reply};

use super::provider::DoubaoProvider;

fn credential() -> Credential {
    Credential::from_value(json!({
        "cookies": "sessionid=s-1; ttwid=t-1; passport_csrf_token=c-1",
        "device_id": "d-1",
        "web_id": "w-1",
        "fp": "f-1"
    }))
}

fn provider(base: &str) -> DoubaoProvider {
    DoubaoProvider::with_base(
        "doubao-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn chat_request() -> ChatRequest {
    ChatRequest::new(
        "doubao",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::text("hello")],
        )],
    )
}

/// One ACK, one thinking container, then text inside a second container.
fn sse_body() -> String {
    let ack = json!({"ack_client_meta": {"conversation_id": "3845"}}).to_string();
    let thinking = json!({
        "patch_op": [{"patch_value": {"content_block": [
            {"block_type": 10040},
            {"block_type": 10000, "content": {"text_block": {"text": "pondering"}}}
        ]}}]
    })
    .to_string();
    let answer = json!({
        "patch_op": [{"patch_value": {"content_block": [
            {"block_type": 10040},
            {"block_type": 10000, "content": {"text_block": {"text": "hello there"}}}
        ]}}]
    })
    .to_string();
    format!(
        "event: SSE_ACK\ndata: {ack}\n\nevent: STREAM_CHUNK\ndata: {thinking}\n\n\
         event: STREAM_CHUNK\ndata: {answer}\n\n"
    )
}

#[tokio::test]
async fn a_stream_is_translated_into_reasoning_and_text() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&credential(), chat_request())
        .await
        .expect("stream");

    let mut reasoning = String::new();
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        match item.expect("event") {
            StreamEvent::ReasoningDelta { text: part } => reasoning.push_str(&part),
            StreamEvent::TextDelta { text: part } => text.push_str(&part),
            _ => {}
        }
    }
    assert_eq!(reasoning, "pondering");
    assert_eq!(text, "hello there");
}

#[tokio::test]
async fn the_request_carries_the_imported_device_identity_and_csrf_header() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&credential(), chat_request())
        .await
        .expect("stream");
    while stream.next().await.is_some() {}

    let sent: &Recorded = &server.requests()[0];
    assert!(sent.target.contains("device_id=d-1"), "{}", sent.target);
    assert!(sent.target.contains("web_id=w-1"), "{}", sent.target);
    assert!(sent.target.contains("fp=f-1"), "{}", sent.target);
    assert!(sent.target.contains("aid=582478"), "{}", sent.target);
    assert_eq!(sent.header("x-tt-passport-csrf-token"), Some("c-1"));
    assert_eq!(
        sent.header("cookie"),
        Some("sessionid=s-1; ttwid=t-1; passport_csrf_token=c-1")
    );
    assert_eq!(
        sent.header("origin"),
        None,
        "the desktop client is not impersonated"
    );
    assert_eq!(sent.header("referer"), None);
}

#[tokio::test]
async fn a_business_error_frame_surfaces_as_a_stream_error() {
    let body =
        "event: STREAM_ERROR\ndata: {\"error_code\":710022004,\"error_msg\":\"rate limited\"}\n\n";
    let server = spawn_server(move |_| Reply::sse(body)).await;
    let provider = provider(&server.base_url);
    let mut stream = provider
        .chat_stream(&credential(), chat_request())
        .await
        .expect("stream");
    let mut kind = None;
    while let Some(item) = stream.next().await {
        if let Ok(StreamEvent::Error { error }) = item {
            kind = Some(error.kind);
        }
    }
    assert_eq!(kind, Some(ErrorKind::RateLimited));
}

#[tokio::test]
async fn a_non_streaming_call_aggregates_the_stream() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let response = provider
        .chat(&credential(), chat_request())
        .await
        .expect("response");
    assert_eq!(response.message.text_content(), "hello there");
    assert_eq!(
        response.model, "doubao",
        "the public model id is echoed, not the upstream tier"
    );
}

#[tokio::test]
async fn a_session_rejected_upstream_is_reported_as_expired() {
    let server =
        spawn_server(|_| Reply::json(401, json!({"code": 10008, "msg": "no login"}).to_string()))
            .await;
    let provider = provider(&server.base_url);
    let error = provider
        .chat_stream(&credential(), chat_request())
        .await
        .err()
        .expect("error");
    assert_eq!(error.kind, ErrorKind::AuthExpired);
    assert_eq!(
        provider.auth_status(&credential()).await.expect("status"),
        AuthStatus::Expired
    );
}

#[tokio::test]
async fn a_credential_without_device_identity_never_reaches_the_network() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let bare = Credential::from_value(json!({
        "cookies": "sessionid=s-1; passport_csrf_token=c-1"
    }));
    let error = provider
        .chat_stream(&bare, chat_request())
        .await
        .err()
        .expect("error");
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
    assert!(error.message.contains("device_id"));
    assert_eq!(server.request_count(), 0, "nothing was sent upstream");
}

#[tokio::test]
async fn the_catalogue_is_the_three_tiers_plus_the_work_models() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let models = provider.list_models(&credential()).await.expect("models");
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "doubao",
            "doubao-think",
            "doubao-expert",
            "doubao-deepseek-v4.1-flash",
            "doubao-glm-5.3",
            "doubao-glm-5.3-flash",
            "doubao-kimi-k3-pro",
            "doubao-kimi-k3-pro-highspeed",
        ]
    );
    assert_eq!(models[2].context_window, Some(256_000));
    assert_eq!(models[4].context_window, Some(300_000));
}

#[tokio::test]
async fn image_input_is_refused_locally() {
    let server = spawn_server(|_| Reply::sse(sse_body())).await;
    let provider = provider(&server.base_url);
    let request = ChatRequest::new(
        "doubao",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![
                ContentBlock::text("what is this"),
                ContentBlock::Unsupported {
                    media_type: "image_url".into(),
                    data: json!({"type": "image_url"}),
                },
            ],
        )],
    );
    let error = provider
        .chat_stream(&credential(), request)
        .await
        .err()
        .expect("error");
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(server.request_count(), 0);
}
