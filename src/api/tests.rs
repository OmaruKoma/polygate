//! Integration tests over the real Axum router.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{router, ApiKeyAuth, ApiState, ModelCatalog};
use crate::accounts::Credential;
use crate::config::{ProviderKind, Region};
use crate::providers::{
    FaultInjection, FaultMode, MockConfig, MockProvider, MockReply, MockToolCall, ProviderKey,
    ProviderRegistry,
};

const KEY: &str = "test-key";

struct TestApp {
    app: Router,
    registry: Arc<ProviderRegistry>,
    mock: Arc<MockProvider>,
}

async fn test_app(config: MockConfig) -> TestApp {
    let mock = Arc::new(MockProvider::new(config));
    let registry = Arc::new(ProviderRegistry::new());
    registry.register(mock.clone()).expect("register mock");
    let catalog = ModelCatalog::discover(&registry, &Credential::from_value(json!({}))).await;
    let state = ApiState::new(
        Arc::clone(&registry),
        catalog,
        ApiKeyAuth::new(vec![KEY.to_string()]).expect("api keys"),
    );
    TestApp {
        app: router(state),
        registry,
        mock,
    }
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

fn get_models(key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/v1/models");
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    builder.body(Body::empty()).expect("request")
}

fn post_chat(body: &Value, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn chat(model: &str, text: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": text}],
        "stream": stream,
    })
}

fn sse_frames(body: &[u8]) -> Vec<String> {
    text_body(body)
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.trim().to_string())
        .filter(|data| !data.is_empty())
        .collect()
}

fn sse_chunks(frames: &[String]) -> Vec<Value> {
    frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str(frame).expect("sse json"))
        .collect()
}

fn ends_with_done(frames: &[String]) -> bool {
    frames.last().map(String::as_str) == Some("[DONE]")
}

fn mock_key() -> ProviderKey {
    ProviderKey::new(ProviderKind::Mock, Region::Cn)
}

// ---------------------------------------------------------------------------
// Health and authentication
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_is_public_and_minimal() {
    let app = test_app(MockConfig::default()).await;
    let request = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .expect("request");
    let (status, _, body) = send(&app.app, request).await;

    assert_eq!(status, StatusCode::OK);
    let value = json_body(&body);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["version"], crate::VERSION);
    let text = text_body(&body);
    assert!(!text.contains(KEY));
    assert!(!text.contains("mock"));
}

#[tokio::test]
async fn valid_bearer_key_is_accepted() {
    let app = test_app(MockConfig::default()).await;
    let (status, _, body) = send(&app.app, get_models(Some(KEY))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["object"], "list");
}

#[tokio::test]
async fn x_api_key_header_is_accepted() {
    let app = test_app(MockConfig::default()).await;
    let request = Request::builder()
        .uri("/v1/models")
        .header("x-api-key", KEY)
        .body(Body::empty())
        .expect("request");
    let (status, _, _) = send(&app.app, request).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn missing_and_wrong_keys_are_rejected() {
    let app = test_app(MockConfig::default()).await;

    for request in [
        get_models(None),
        get_models(Some("wrong-key")),
        post_chat(&chat("mock-chat", "hi", false), None),
    ] {
        let (status, headers, body) = send(&app.app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let value = json_body(&body);
        assert_eq!(value["error"]["code"], "invalid_api_key");
        assert!(value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("API key"));
    }
    assert_eq!(app.mock.request_count(), 0);
}

#[tokio::test]
async fn empty_keys_are_rejected() {
    let app = test_app(MockConfig::default()).await;

    for (name, value) in [("authorization", "Bearer"), ("x-api-key", "")] {
        let request = Request::builder()
            .uri("/v1/models")
            .header(name, value)
            .body(Body::empty())
            .expect("request");
        let (status, _, body) = send(&app.app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
        assert_eq!(json_body(&body)["error"]["code"], "invalid_api_key");
    }

    // A malformed Authorization header does not fall back to x-api-key.
    let request = Request::builder()
        .uri("/v1/models")
        .header(header::AUTHORIZATION, "Basic abc")
        .header("x-api-key", KEY)
        .body(Body::empty())
        .expect("request");
    let (status, _, _) = send(&app.app, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Model listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn models_list_mock_models_as_test_models() {
    let app = test_app(MockConfig::default()).await;
    let (status, _, body) = send(&app.app, get_models(Some(KEY))).await;
    assert_eq!(status, StatusCode::OK);

    let value = json_body(&body);
    assert_eq!(value["object"], "list");
    let models = value["data"].as_array().expect("data array");
    let ids: Vec<&str> = models
        .iter()
        .map(|model| model["id"].as_str().expect("model id"))
        .collect();
    assert_eq!(ids, vec!["mock-chat", "mock-reasoner"]);

    for model in models {
        assert_eq!(model["object"], "model");
        assert_eq!(model["owned_by"], "mock-cn");
        assert_eq!(model["polygate"]["mock"], true);
        assert_eq!(model["polygate"]["test_model"], true);
        assert_eq!(model["polygate"]["verification"], "experimental");
        assert_eq!(model["polygate"]["region"], "cn");
    }
}

#[tokio::test]
async fn disabled_providers_are_hidden_and_not_routable() {
    let app = test_app(MockConfig::default()).await;
    app.registry
        .set_enabled(&mock_key(), false)
        .expect("disable mock");

    let (status, _, body) = send(&app.app, get_models(Some(KEY))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_body(&body)["data"]
        .as_array()
        .expect("data")
        .is_empty());

    let (status, _, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", false), Some(KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_body(&body)["error"]["code"], "model_unavailable");
    assert_eq!(app.mock.request_count(), 0);
}

#[tokio::test]
async fn unknown_model_is_404_without_upstream_call() {
    let app = test_app(MockConfig::default()).await;
    let (status, _, body) =
        send(&app.app, post_chat(&chat("gpt-4o", "hi", false), Some(KEY))).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    let value = json_body(&body);
    assert_eq!(value["error"]["code"], "model_unavailable");
    assert_eq!(app.mock.request_count(), 0);
}

// ---------------------------------------------------------------------------
// Non-streaming chat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_streaming_chat_returns_openai_response() {
    let app =
        test_app(MockConfig::default().with_reply(MockReply::fixed("hello from the gateway")))
            .await;
    let (status, headers, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", false), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let value = json_body(&body);
    assert_eq!(value["object"], "chat.completion");
    assert!(value["id"].as_str().expect("id").starts_with("chatcmpl-"));
    assert_eq!(value["model"], "mock-chat");
    assert_eq!(value["choices"][0]["index"], 0);
    assert_eq!(value["choices"][0]["message"]["role"], "assistant");
    assert_eq!(
        value["choices"][0]["message"]["content"],
        "hello from the gateway"
    );
    assert_eq!(value["choices"][0]["finish_reason"], "stop");
    assert_eq!(value["usage"]["prompt_tokens"], 1);
    assert_eq!(value["usage"]["completion_tokens"], 6);
    assert_eq!(value["usage"]["total_tokens"], 7);
    assert_eq!(app.mock.request_count(), 1);
}

#[tokio::test]
async fn non_streaming_tool_calls_are_preserved() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed(""))
            .with_tool_call(MockToolCall::new(
                "call-1",
                "read_file",
                json!({ "path": "src/lib.rs" }),
            )),
    )
    .await;

    let body = json!({
        "model": "mock-chat",
        "messages": [
            {"role": "user", "content": "read the file"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-0",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call-0", "content": "file body"}
        ],
        "tools": [{
            "type": "function",
            "function": {"name": "read_file", "parameters": {"type": "object"}}
        }],
        "tool_choice": "auto"
    });
    let (status, _, response) = send(&app.app, post_chat(&body, Some(KEY))).await;

    assert_eq!(status, StatusCode::OK);
    let value = json_body(&response);
    assert_eq!(value["choices"][0]["message"]["content"], Value::Null);
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    let call = &value["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["id"], "call-1");
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "read_file");
    assert_eq!(
        serde_json::from_str::<Value>(call["function"]["arguments"].as_str().expect("arguments"))
            .expect("json"),
        json!({ "path": "src/lib.rs" })
    );
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sse_streams_text_chunks_and_done() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed("abcdefghij"))
            .with_chunk_size(4),
    )
    .await;
    let (status, headers, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", true), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    let frames = sse_frames(&body);
    assert!(ends_with_done(&frames));
    let chunks = sse_chunks(&frames);
    assert_eq!(chunks[0]["object"], "chat.completion.chunk");
    assert_eq!(chunks[0]["model"], "mock-chat");
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");

    let content: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "abcdefghij");

    let finish: Vec<&str> = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["finish_reason"].as_str())
        .collect();
    assert_eq!(finish, vec!["stop"]);
    assert!(chunks[0]["id"]
        .as_str()
        .expect("id")
        .starts_with("chatcmpl-"));
}

#[tokio::test]
async fn sse_streams_tool_call_deltas_with_indices() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed(""))
            .with_tool_call(MockToolCall::new(
                "call-a",
                "read_file",
                json!({ "path": "a.txt" }),
            ))
            .with_chunk_size(4),
    )
    .await;
    let (status, _, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", true), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let frames = sse_frames(&body);
    assert!(ends_with_done(&frames));
    let chunks = sse_chunks(&frames);

    let starts: Vec<&Value> = chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"]
                .as_array()?
                .first()
        })
        .filter(|call| call.get("id").is_some())
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["index"], 0);
    assert_eq!(starts[0]["id"], "call-a");
    assert_eq!(starts[0]["type"], "function");
    assert_eq!(starts[0]["function"]["name"], "read_file");
    assert_eq!(starts[0]["function"]["arguments"], "");

    let mut arguments = String::new();
    for chunk in &chunks {
        if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
            for call in calls {
                assert_eq!(call["index"], 0);
                if let Some(fragment) = call["function"]["arguments"].as_str() {
                    arguments.push_str(fragment);
                }
            }
        }
    }
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).expect("json"),
        json!({ "path": "a.txt" })
    );

    let finish: Vec<&str> = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["finish_reason"].as_str())
        .collect();
    assert_eq!(finish, vec!["tool_calls"]);
}

#[tokio::test]
async fn sse_interleaves_multiple_tool_calls() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed(""))
            .with_tool_call(MockToolCall::new(
                "call-a",
                "read_file",
                json!({ "path": "a.txt" }),
            ))
            .with_tool_call(MockToolCall::new(
                "call-b",
                "read_file",
                json!({ "path": "b.txt" }),
            ))
            .with_chunk_size(4),
    )
    .await;
    let (status, _, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", true), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let chunks = sse_chunks(&sse_frames(&body));

    let starts: Vec<(u64, String)> = chunks
        .iter()
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"]
                .as_array()?
                .first()
        })
        .filter(|call| call.get("id").is_some())
        .map(|call| {
            (
                call["index"].as_u64().expect("index"),
                call["function"]["name"].as_str().expect("name").to_string(),
            )
        })
        .collect();
    assert_eq!(
        starts,
        vec![(0, "read_file".to_string()), (1, "read_file".to_string())]
    );

    let mut fragments: [String; 2] = [String::new(), String::new()];
    let mut order = Vec::new();
    for chunk in &chunks {
        if let Some(calls) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
            for call in calls {
                if let Some(fragment) = call["function"]["arguments"].as_str() {
                    let index = call["index"].as_u64().expect("index") as usize;
                    order.push(index);
                    fragments[index].push_str(fragment);
                }
            }
        }
    }
    assert_eq!(&order[..4], [0, 1, 0, 1]);
    assert_eq!(
        serde_json::from_str::<Value>(&fragments[0]).expect("json"),
        json!({ "path": "a.txt" })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&fragments[1]).expect("json"),
        json!({ "path": "b.txt" })
    );

    let finish: Vec<&str> = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["finish_reason"].as_str())
        .collect();
    assert_eq!(finish, vec!["tool_calls"]);
}

#[tokio::test]
async fn sse_reports_usage_when_requested() {
    let app = test_app(MockConfig::default().with_reply(MockReply::fixed("hello"))).await;
    let body = json!({
        "model": "mock-chat",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    let (status, _, response) = send(&app.app, post_chat(&body, Some(KEY))).await;

    assert_eq!(status, StatusCode::OK);
    let frames = sse_frames(&response);
    assert!(ends_with_done(&frames));
    let chunks = sse_chunks(&frames);

    let usage_chunk = chunks
        .iter()
        .find(|chunk| !chunk["usage"].is_null())
        .expect("usage chunk");
    assert!(usage_chunk["choices"]
        .as_array()
        .expect("choices")
        .is_empty());
    assert_eq!(usage_chunk["usage"]["prompt_tokens"], 1);
    assert_eq!(usage_chunk["usage"]["completion_tokens"], 2);
    assert_eq!(usage_chunk["usage"]["total_tokens"], 3);

    // Usage arrives after the finish chunk and before [DONE].
    assert_eq!(
        chunks
            .iter()
            .position(|chunk| chunk["choices"][0]["finish_reason"].as_str() == Some("stop")),
        Some(chunks.len() - 2)
    );
}

#[tokio::test]
async fn sse_mid_stream_fault_is_reported_as_an_error_frame() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed("abcdefgh"))
            .with_chunk_size(2)
            .with_fault(FaultInjection::new(FaultMode::UpstreamServer).after_events(2)),
    )
    .await;
    let (status, _, body) = send(
        &app.app,
        post_chat(&chat("mock-chat", "hi", true), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let frames = sse_frames(&body);
    assert!(ends_with_done(&frames));
    let chunks = sse_chunks(&frames);
    let error = chunks.last().expect("error frame");
    assert!(error["choices"].is_null());
    assert_eq!(error["error"]["code"], "upstream_error");
    assert!(error["error"]["message"].is_string());
    assert_eq!(app.mock.in_flight_requests(), 0);
}

// ---------------------------------------------------------------------------
// Provider error mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn provider_faults_map_to_openai_errors() {
    let cases = [
        (
            FaultMode::AuthInvalid,
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
        ),
        (
            FaultMode::QuotaExhausted,
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_quota",
        ),
        (
            FaultMode::RateLimited,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
        ),
        (
            FaultMode::UpstreamServer,
            StatusCode::BAD_GATEWAY,
            "upstream_error",
        ),
    ];

    for (mode, expected_status, expected_code) in cases {
        let app = test_app(MockConfig::default().with_fault(FaultInjection::new(mode))).await;

        let (status, headers, body) = send(
            &app.app,
            post_chat(&chat("mock-chat", "hi", false), Some(KEY)),
        )
        .await;
        assert_eq!(status, expected_status, "{mode:?}");
        assert_eq!(json_body(&body)["error"]["code"], expected_code);
        if mode == FaultMode::RateLimited {
            assert_eq!(
                headers
                    .get(header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
                Some("1")
            );
        }

        // Streaming requests fail before the SSE body starts.
        let (status, _, body) = send(
            &app.app,
            post_chat(&chat("mock-chat", "hi", true), Some(KEY)),
        )
        .await;
        assert_eq!(status, expected_status, "{mode:?}");
        assert_eq!(json_body(&body)["error"]["code"], expected_code);
    }
}

// ---------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_requests_are_rejected() {
    let app = test_app(MockConfig::default()).await;

    let cases = [
        json!({"model": "", "messages": [{"role": "user", "content": "hi"}]}),
        json!({"model": "mock-chat", "messages": []}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "n": 2}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "logprobs": true}),
        json!({"model": "mock-chat", "messages": [{"role": "system"}]}),
        json!({"model": "mock-chat", "messages": [{"role": "tool", "content": "x"}]}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "tool_choice": {"type": "function", "function": {"name": "read_file"}}}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "temperature": 5}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "top_p": 2}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 0}),
        json!({"model": "mock-chat", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 10, "max_completion_tokens": 10}),
        json!({"model": "mock-chat", "messages": [{"role": "robot", "content": "hi"}]}),
    ];

    for body in cases {
        let (status, _, response) = send(&app.app, post_chat(&body, Some(KEY))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            json_body(&response)["error"]["code"],
            "invalid_request_error",
            "{body}"
        );
    }

    // Malformed JSON is rejected with the same envelope.
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .expect("request");
    let (status, _, response) = send(&app.app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(&response)["error"]["code"],
        "invalid_request_error"
    );

    assert_eq!(app.mock.request_count(), 0);
}

// ---------------------------------------------------------------------------
// Cancellation and concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_disconnect_cancels_the_provider_stream() {
    let app = test_app(
        MockConfig::default()
            .with_reply(MockReply::fixed("0123456789abcdef"))
            .with_chunk_size(2)
            .with_event_delay(Duration::from_millis(5)),
    )
    .await;

    let response = app
        .app
        .clone()
        .oneshot(post_chat(&chat("mock-chat", "hi", true), Some(KEY)))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(app.mock.in_flight_requests(), 1);

    let mut body = response.into_body();
    let frame = body
        .frame()
        .await
        .expect("first frame")
        .expect("frame data");
    assert!(frame.data_ref().is_some());

    drop(body);
    assert_eq!(
        app.mock.in_flight_requests(),
        0,
        "dropping the response body must release the provider stream"
    );
}

#[tokio::test]
async fn concurrent_requests_do_not_interfere() {
    let app = test_app(MockConfig::default().with_reply(MockReply::echo("echo: "))).await;

    let first = app
        .app
        .clone()
        .oneshot(post_chat(&chat("mock-chat", "alpha", false), Some(KEY)));
    let second = app
        .app
        .clone()
        .oneshot(post_chat(&chat("mock-chat", "beta", false), Some(KEY)));
    let (first, second) = tokio::join!(first, second);

    let first = json_body(
        &to_bytes(first.expect("first").into_body(), 16 * 1024 * 1024)
            .await
            .expect("body"),
    );
    let second = json_body(
        &to_bytes(second.expect("second").into_body(), 16 * 1024 * 1024)
            .await
            .expect("body"),
    );

    let mut texts = vec![
        first["choices"][0]["message"]["content"]
            .as_str()
            .expect("first text")
            .to_string(),
        second["choices"][0]["message"]["content"]
            .as_str()
            .expect("second text")
            .to_string(),
    ];
    texts.sort();
    assert_eq!(
        texts,
        vec!["echo: alpha".to_string(), "echo: beta".to_string()]
    );
    assert_eq!(app.mock.request_count(), 2);
}

#[test]
fn session_id_prefers_the_header_and_otherwise_uses_a_stable_prefix() {
    use super::session_id_for;
    use super::wire::WireRequest;
    use super::ChatCompletionRequest;

    let request: ChatCompletionRequest =
        serde_json::from_value(chat("mock-chat", "hello", false)).expect("request");
    let headers = HeaderMap::new();
    let first =
        session_id_for(&headers, request.model(), request.first_user_text()).expect("auto id");
    let second =
        session_id_for(&headers, request.model(), request.first_user_text()).expect("auto id");
    assert_eq!(first, second);
    assert!(first.starts_with("auto-"));

    let mut headers = HeaderMap::new();
    headers.insert("x-session-id", "client-supplied".parse().expect("header"));
    assert_eq!(
        session_id_for(&headers, request.model(), request.first_user_text()).as_deref(),
        Some("client-supplied")
    );
}
