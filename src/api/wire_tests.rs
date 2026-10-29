//! `/v1/responses` and `/v1/messages` wire-surface tests.
//!
//! These drive the real Axum router against the deterministic mock provider:
//! no network, no credentials. They assert wire shape, streaming event order
//! and honest rejection of unsupported parameters.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{router, ApiKeyAuth, ApiState, ModelCatalog};
use crate::accounts::Credential;
use crate::config::Region;
use crate::providers::{MockConfig, MockProvider, MockReply, MockToolCall, ProviderRegistry};

const KEY: &str = "test-key";

async fn test_app(config: MockConfig) -> Router {
    let mock = Arc::new(MockProvider::new(config));
    let registry = Arc::new(ProviderRegistry::new());
    registry.register(mock).expect("register mock");
    let catalog = ModelCatalog::discover(&registry, &Credential::from_value(json!({}))).await;
    let state = ApiState::new(
        registry,
        catalog,
        ApiKeyAuth::new(vec![KEY.to_string()]).expect("api keys"),
    );
    router(state)
}

fn post_json(uri: &str, body: &Value, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("response body");
    (status, headers, body.to_vec())
}

fn json_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("json body")
}

fn text_body(body: &[u8]) -> String {
    String::from_utf8(body.to_vec()).expect("utf8 body")
}

/// Parse SSE frames into `(event, data)` pairs.
fn sse_events(body: &[u8]) -> Vec<(String, Value)> {
    let mut events = Vec::new();
    let mut current: Option<String> = None;
    for line in text_body(body).lines() {
        if let Some(name) = line.strip_prefix("event:") {
            current = Some(name.trim().to_string());
        } else if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(data).expect("sse json");
            let name = current.take().unwrap_or_else(|| {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            });
            events.push((name, value));
        }
    }
    events
}

async fn echo_app() -> Router {
    test_app(MockConfig::new("mock-cn", Region::Cn).with_reply(MockReply::echo("echo: "))).await
}

// ---------------------------------------------------------------------------
// Responses API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn responses_non_streaming_returns_output_text_and_usage() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({"model": "mock-chat", "input": "hello"}),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = json_body(&body);
    assert_eq!(value["object"], "response");
    assert_eq!(value["status"], "completed");
    assert_eq!(value["model"], "mock-chat");
    assert_eq!(value["output_text"], "echo: hello");
    assert_eq!(value["output"][0]["type"], "message");
    assert_eq!(value["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(value["output"][0]["content"][0]["text"], "echo: hello");
    assert!(value["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0);
    assert!(value["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn responses_streaming_emits_created_deltas_and_completed() {
    let app = echo_app().await;
    let (status, headers, body) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({"model": "mock-chat", "input": "hello", "stream": true}),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .starts_with("text/event-stream"));
    let events = sse_events(&body);
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.first(), Some(&"response.created"));
    assert_eq!(names.last(), Some(&"response.completed"));
    let deltas: String = events
        .iter()
        .filter(|(name, _)| name == "response.output_text.delta")
        .filter_map(|(_, data)| data["delta"].as_str())
        .collect();
    assert_eq!(deltas, "echo: hello");
    let completed = events
        .iter()
        .find(|(name, _)| name == "response.completed")
        .map(|(_, data)| data)
        .expect("completed event");
    assert_eq!(
        completed["response"]["output"][0]["content"][0]["text"],
        "echo: hello"
    );
    assert!(
        completed["response"]["usage"]["output_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn responses_function_calls_round_trip() {
    let app = test_app(
        MockConfig::new("mock-cn", Region::Cn)
            .with_reply(MockReply::fixed("checking"))
            .with_tool_call(MockToolCall::new(
                "call-1",
                "get_weather",
                json!({"city": "Paris"}),
            )),
    )
    .await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({
                "model": "mock-chat",
                "input": [{"role": "user", "content": [{"type": "input_text", "text": "weather?"}]}],
                "tools": [{
                    "type": "function",
                    "name": "get_weather",
                    "description": "Look up the weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }],
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = json_body(&body);
    let call = value["output"]
        .as_array()
        .expect("output array")
        .iter()
        .find(|item| item["type"] == "function_call")
        .expect("function_call item");
    assert_eq!(call["call_id"], "call-1");
    assert_eq!(call["name"], "get_weather");
    let arguments: Value =
        serde_json::from_str(call["arguments"].as_str().expect("arguments string"))
            .expect("arguments json");
    assert_eq!(arguments["city"], "Paris");
}

#[tokio::test]
async fn responses_accepts_function_call_output_turns() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({
                "model": "mock-chat",
                "input": [
                    {"role": "user", "content": "weather?"},
                    {"type": "function_call", "call_id": "call-1", "name": "get_weather", "arguments": "{}"},
                    {"type": "function_call_output", "call_id": "call-1", "output": "sunny"}
                ]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["status"], "completed");
}

#[tokio::test]
async fn responses_rejects_unsupported_state_and_params() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({
                "model": "mock-chat",
                "input": "hello",
                "previous_response_id": "resp-1"
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json_body(&body)["error"]["code"], "invalid_request_error");
}

#[tokio::test]
async fn responses_requires_a_gateway_key() {
    let app = echo_app().await;
    let (status, _, _) = send(
        &app,
        post_json(
            "/v1/responses",
            &json!({"model": "mock-chat", "input": "hello"}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Anthropic Messages API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn messages_non_streaming_returns_anthropic_message() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 64,
                "system": "be brief",
                "messages": [{"role": "user", "content": "hello"}]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = json_body(&body);
    assert_eq!(value["type"], "message");
    assert_eq!(value["role"], "assistant");
    assert_eq!(value["content"][0]["type"], "text");
    assert_eq!(value["content"][0]["text"], "echo: hello");
    assert_eq!(value["stop_reason"], "end_turn");
    assert!(value["usage"]["output_tokens"].as_u64().unwrap_or(0) > 0);
}

#[tokio::test]
async fn messages_streaming_emits_anthropic_events() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = sse_events(&body);
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names.first(), Some(&"message_start"));
    assert!(names.contains(&"content_block_start"));
    assert!(names.contains(&"content_block_delta"));
    assert!(names.contains(&"content_block_stop"));
    assert_eq!(names[names.len() - 2], "message_delta");
    assert_eq!(names.last(), Some(&"message_stop"));

    let text: String = events
        .iter()
        .filter(|(name, _)| name == "content_block_delta")
        .filter_map(|(_, data)| data["delta"]["text"].as_str())
        .collect();
    assert_eq!(text, "echo: hello");
    let delta = events
        .iter()
        .find(|(name, _)| name == "message_delta")
        .map(|(_, data)| data)
        .expect("message_delta");
    assert_eq!(delta["delta"]["stop_reason"], "end_turn");
}

#[tokio::test]
async fn messages_tool_use_and_tool_result() {
    let app = test_app(
        MockConfig::new("mock-cn", Region::Cn)
            .with_reply(MockReply::fixed("done"))
            .with_tool_call(MockToolCall::new(
                "toolu_1",
                "get_weather",
                json!({"city": "Paris"}),
            )),
    )
    .await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 64,
                "tools": [{
                    "name": "get_weather",
                    "description": "weather",
                    "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
                }],
                "messages": [{"role": "user", "content": "weather?"}]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = json_body(&body);
    let tool = value["content"]
        .as_array()
        .expect("content")
        .iter()
        .find(|block| block["type"] == "tool_use")
        .expect("tool_use block");
    assert_eq!(tool["id"], "toolu_1");
    assert_eq!(tool["name"], "get_weather");
    assert_eq!(tool["input"]["city"], "Paris");
    assert_eq!(value["stop_reason"], "tool_use");

    // The client returns the result in a user turn.
    let (status, _, _) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 64,
                "messages": [
                    {"role": "user", "content": "weather?"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny"}
                    ]}
                ]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn messages_rejects_unknown_model_with_anthropic_error() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "does-not-exist",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let value = json_body(&body);
    assert_eq!(value["type"], "error");
    assert_eq!(value["error"]["type"], "not_found_error");
}

#[tokio::test]
async fn messages_rejects_top_k_explicitly() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 16,
                "top_k": 5,
                "messages": [{"role": "user", "content": "hi"}]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let value = json_body(&body);
    assert_eq!(value["error"]["type"], "invalid_request_error");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("top_k"));
}

#[tokio::test]
async fn messages_vision_gate_refuses_unsupported_models() {
    let app = echo_app().await;
    let (status, _, body) = send(
        &app,
        post_json(
            "/v1/messages",
            &json!({
                "model": "mock-chat",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
                ]}]
            }),
            Some(KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = json_body(&body)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("vision"),
        "capability gate should mention vision: {message}"
    );
}
