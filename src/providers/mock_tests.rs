//! Behavioural tests for the deterministic mock provider.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use super::mock::{
    FaultInjection, FaultMode, FaultScope, MockConfig, MockProvider, MockReply, MockToolCall,
};
use super::{CancelRequest, Capability, ChatStream, Provider, ProviderRegistry};
use crate::accounts::Credential;
use crate::config::Region;
use crate::error::ErrorKind;
use crate::protocol::{
    ChatMessage, ChatRequest, FinishReason, StreamEvent, StreamEventValidator, StreamState,
    ToolDefinition,
};

fn credential() -> Credential {
    Credential::from_value(serde_json::json!({ "token": "mock-only" }))
}

fn request(model: &str, text: &str) -> ChatRequest {
    ChatRequest::new(model, vec![ChatMessage::user(text)])
}

fn request_with_id(text: &str, id: &str) -> ChatRequest {
    let mut request = request("mock-chat", text);
    request.request_id = Some(id.to_string());
    request
}

async fn drain(mut stream: ChatStream) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item"));
    }
    events
}

async fn collect(provider: &MockProvider, request: ChatRequest) -> Vec<StreamEvent> {
    let stream = provider
        .chat_stream(&credential(), request)
        .await
        .expect("stream must start");
    tokio::time::timeout(Duration::from_secs(5), drain(stream))
        .await
        .expect("stream must terminate")
}

async fn chat_error(config: MockConfig) -> crate::error::GatewayError {
    MockProvider::new(config)
        .chat(&credential(), request("mock-chat", "hi"))
        .await
        .expect_err("expected an injected failure")
}

async fn stream_error(config: MockConfig) -> crate::error::GatewayError {
    let provider = MockProvider::new(config);
    match provider
        .chat_stream(&credential(), request("mock-chat", "hi"))
        .await
    {
        Ok(_) => panic!("expected an injected failure"),
        Err(err) => err,
    }
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_argument_fragments(events: &[StreamEvent]) -> (Vec<String>, BTreeMap<String, String>) {
    let mut order = Vec::new();
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    for event in events {
        if let StreamEvent::ToolCallArgsDelta { id, delta } = event {
            order.push(id.clone());
            by_id.entry(id.clone()).or_default().push_str(delta);
        }
    }
    (order, by_id)
}

fn validate(events: &[StreamEvent]) -> StreamEventValidator {
    let mut validator = StreamEventValidator::new();
    for event in events {
        validator
            .validate(event)
            .expect("events must follow the canonical order");
    }
    validator
}

// ---------------------------------------------------------------------------
// Registration and capability declaration
// ---------------------------------------------------------------------------

#[test]
fn mock_provider_registers_with_the_registry() {
    let registry = ProviderRegistry::new();
    let info = registry
        .register(MockProvider::shared(MockConfig::default()))
        .expect("register");

    assert!(info.is_mock());
    assert_eq!(info.kind, crate::config::ProviderKind::Mock);
    assert_eq!(info.region, Region::Cn);
    assert_eq!(info.verification, super::VerificationState::Experimental);
    assert_ne!(info.verification, super::VerificationState::Verified);

    let key = info.key();
    for capability in [
        Capability::ModelDiscovery,
        Capability::Chat,
        Capability::Streaming,
        Capability::ToolCalling,
        Capability::Cancellation,
    ] {
        assert!(
            registry.require_capability(&key, capability).is_ok(),
            "{capability} must be declared"
        );
    }
    for capability in [Capability::AgentTask, Capability::NativeSession] {
        let err = registry
            .require_capability(&key, capability)
            .expect_err("capability must stay undeclared");
        assert_eq!(err.kind, ErrorKind::Unsupported, "{capability}");
    }
    assert_eq!(registry.list_enabled().len(), 1);
}

#[tokio::test]
async fn list_models_returns_the_configured_catalogue() {
    let provider = MockProvider::development();
    let models = provider
        .list_models(&credential())
        .await
        .expect("model discovery");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["mock-chat", "mock-reasoner"]);
}

#[tokio::test]
async fn unknown_model_is_rejected() {
    let provider = MockProvider::development();
    let err = provider
        .chat(&credential(), request("gpt-4o", "hi"))
        .await
        .expect_err("unknown model");
    assert_eq!(err.kind, ErrorKind::ModelUnavailable);
    assert_eq!(err.kind.http_status().as_u16(), 404);
}

// ---------------------------------------------------------------------------
// Non-streaming responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_streaming_chat_is_deterministic() {
    let config = MockConfig::default().with_reply(MockReply::fixed("hello from mock"));
    let first = MockProvider::new(config.clone());
    let second = MockProvider::new(config);

    let a = first
        .chat(&credential(), request("mock-chat", "hi"))
        .await
        .expect("chat");
    let b = second
        .chat(&credential(), request("mock-chat", "hi"))
        .await
        .expect("chat");

    assert_eq!(a.id, b.id);
    assert_eq!(a.message, b.message);
    assert_eq!(a.finish_reason, b.finish_reason);
    assert_eq!(a.usage, b.usage);
    assert_eq!(a.text(), "hello from mock");
    assert_eq!(a.finish_reason, FinishReason::Stop);
    assert_eq!(a.model, "mock-chat");

    let usage = a.usage.expect("usage is always reported");
    assert_eq!(usage.input_tokens, 1); // "hi" -> ceil(2 / 4)
    assert_eq!(usage.output_tokens, 4); // 15 chars -> ceil(15 / 4)
    assert_eq!(usage.total_tokens, Some(5));
    assert_eq!(usage.reasoning_tokens, None);
}

#[tokio::test]
async fn non_streaming_echoes_the_last_user_turn() {
    let provider = MockProvider::new(MockConfig::default().with_reply(MockReply::echo("echo: ")));
    let request = ChatRequest::new(
        "mock-chat",
        vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ],
    );

    let response = provider
        .chat(&credential(), request)
        .await
        .expect("multi-turn chat");
    assert_eq!(response.text(), "echo: second question");
    assert_eq!(
        response.message.role,
        crate::protocol::MessageRole::Assistant
    );
    // 49 characters across every turn -> ceil(49 / 4)
    assert_eq!(response.usage.expect("usage").input_tokens, 13);
}

#[tokio::test]
async fn non_streaming_tool_calls() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed(""))
            .with_tool_call(MockToolCall::new(
                "call-1",
                "read_file",
                serde_json::json!({ "path": "src/lib.rs" }),
            )),
    );
    let mut request = request("mock-chat", "read the crate root");
    request
        .tools
        .push(ToolDefinition::new("read_file", serde_json::json!({})));

    let response = provider.chat(&credential(), request).await.expect("chat");
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert!(response.text().is_empty());

    let calls: Vec<_> = response.message.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call-1");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(
        calls[0].parse_arguments().expect("valid json"),
        serde_json::json!({ "path": "src/lib.rs" })
    );
    assert!(response.usage.expect("usage").output_tokens > 0);
}

// ---------------------------------------------------------------------------
// Streaming responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_text_events_are_ordered_and_validated() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed("abcdefghij"))
            .with_chunk_size(4),
    );
    let events = collect(&provider, request("mock-chat", "hi")).await;

    let chunks: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    assert_eq!(text_of(&events), "abcdefghij");
    assert_eq!(events.len(), 6);
    assert!(matches!(events[0], StreamEvent::Start { .. }));
    assert!(matches!(events[4], StreamEvent::Usage { .. }));
    assert!(matches!(
        events[5],
        StreamEvent::Finish {
            reason: FinishReason::Stop
        }
    ));

    let validator = validate(&events);
    validator.ensure_terminated().expect("stream terminated");
    assert_eq!(validator.state(), StreamState::Finished);
    assert_eq!(validator.usage().map(|usage| usage.input_tokens), Some(1));
}

#[tokio::test]
async fn streaming_tool_calls_interleave_argument_fragments() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed(""))
            .with_tool_call(MockToolCall::new(
                "call-a",
                "read_file",
                serde_json::json!({ "path": "a.txt" }),
            ))
            .with_tool_call(MockToolCall::new(
                "call-b",
                "read_file",
                serde_json::json!({ "path": "b.txt" }),
            ))
            .with_chunk_size(4),
    );
    let events = collect(&provider, request("mock-chat", "read both files")).await;

    let starts: Vec<(String, String, Option<u32>)> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallStart { id, name, index } => {
                Some((id.clone(), name.clone(), *index))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![
            ("call-a".to_string(), "read_file".to_string(), Some(0)),
            ("call-b".to_string(), "read_file".to_string(), Some(1)),
        ]
    );

    let (order, by_id) = tool_argument_fragments(&events);
    let ids: Vec<&str> = order.iter().map(String::as_str).collect();
    assert_eq!(&ids[..4], ["call-a", "call-b", "call-a", "call-b"]);
    assert!(ids.contains(&"call-a"));
    assert!(ids.contains(&"call-b"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&by_id["call-a"]).expect("valid json"),
        serde_json::json!({ "path": "a.txt" })
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&by_id["call-b"]).expect("valid json"),
        serde_json::json!({ "path": "b.txt" })
    );
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finish {
            reason: FinishReason::ToolCalls
        })
    ));

    let validator = validate(&events);
    validator.ensure_terminated().expect("stream terminated");
}

#[tokio::test]
async fn streaming_usage_and_finish_are_terminal() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed("done"))
            .with_reasoning("think")
            .with_chunk_size(2),
    );
    let events = collect(&provider, request("mock-chat", "hi")).await;

    assert_eq!(reasoning_of(&events), "think");
    assert_eq!(text_of(&events), "done");

    let usage_events = events
        .iter()
        .filter(|event| matches!(event, StreamEvent::Usage { .. }))
        .count();
    assert_eq!(usage_events, 1);
    let usage = match &events[events.len() - 2] {
        StreamEvent::Usage { usage } => *usage,
        other => panic!("expected usage before finish, got {other:?}"),
    };
    assert_eq!(usage.reasoning_tokens, Some(2)); // ceil(5 / 4)
    assert_eq!(usage.output_tokens, 3); // "done" (1) + reasoning (2)

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Finish {
            reason: FinishReason::Stop
        })
    ));

    let validator = validate(&events);
    assert_eq!(validator.usage(), Some(usage));
    validator.ensure_terminated().expect("stream terminated");
}

// ---------------------------------------------------------------------------
// Fault injection: failures before the stream starts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_invalid_fault_returns_401() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::AuthInvalid));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.kind.http_status().as_u16(), 401);
    assert!(err.kind.blames_account());

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::AuthInvalid);
    assert_eq!(err.kind.http_status().as_u16(), 401);

    // A chat-only fault leaves streaming healthy.
    let provider = MockProvider::new(
        MockConfig::default()
            .with_fault(FaultInjection::new(FaultMode::AuthInvalid).scope(FaultScope::Chat)),
    );
    let events = collect(&provider, request("mock-chat", "hi")).await;
    validate(&events).ensure_terminated().expect("terminated");
}

#[tokio::test]
async fn quota_exhausted_fault_returns_402() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::QuotaExhausted));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::QuotaExhausted);
    assert_eq!(err.kind.http_status().as_u16(), 402);
    assert!(!err.kind.retryable());

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::QuotaExhausted);
    assert_eq!(err.kind.http_status().as_u16(), 402);
}

#[tokio::test]
async fn rate_limited_fault_returns_429() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::RateLimited));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert_eq!(err.kind.http_status().as_u16(), 429);
    assert_eq!(err.retry_after, Some(1));
    assert!(err.kind.retryable());

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert_eq!(err.retry_after, Some(1));
}

#[tokio::test]
async fn upstream_server_fault_records_upstream_status_500() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::UpstreamServer));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::UpstreamServer);
    // The gateway exposes 502 (upstream failure); the injected upstream status
    // is recorded separately.
    assert_eq!(err.kind.http_status().as_u16(), 502);
    assert_eq!(err.upstream_status, Some(500));
    assert!(err.kind.retryable());

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::UpstreamServer);
    assert_eq!(err.upstream_status, Some(500));
}

#[tokio::test]
async fn timeout_fault_returns_504() {
    let config = MockConfig::default()
        .with_fault(FaultInjection::new(FaultMode::Timeout).timeout(Duration::from_millis(5)));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert_eq!(err.kind.http_status().as_u16(), 504);

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert_eq!(err.kind.http_status().as_u16(), 504);
}

#[tokio::test]
async fn transport_fault_is_classified() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::Transport));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::Transport);
    assert_eq!(err.kind.http_status().as_u16(), 502);

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::Transport);
}

#[tokio::test]
async fn cancelled_fault_reports_499() {
    let config = MockConfig::default().with_fault(FaultInjection::new(FaultMode::Cancelled));
    let err = chat_error(config.clone()).await;
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert_eq!(err.kind.http_status().as_u16(), 499);

    let err = stream_error(config).await;
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn faults_target_a_single_request_by_ordinal() {
    let provider = MockProvider::new(
        MockConfig::default().with_fault(FaultInjection::new(FaultMode::RateLimited).at(1)),
    );
    let first = provider
        .chat(&credential(), request("mock-chat", "first"))
        .await
        .expect("first request succeeds");
    assert_eq!(first.text(), "mock: first");

    let err = provider
        .chat(&credential(), request("mock-chat", "second"))
        .await
        .expect_err("second request hits the fault");
    assert_eq!(err.kind, ErrorKind::RateLimited);

    let third = provider
        .chat(&credential(), request("mock-chat", "third"))
        .await
        .expect("third request succeeds");
    assert_eq!(third.text(), "mock: third");
}

// ---------------------------------------------------------------------------
// Fault injection: mid-stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mid_stream_fault_emits_a_terminal_error() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed("abcdefgh"))
            .with_chunk_size(2)
            .with_fault(FaultInjection::new(FaultMode::UpstreamServer).after_events(3)),
    );
    let events = collect(&provider, request("mock-chat", "hi")).await;

    assert_eq!(events.len(), 4); // Start, 2 deltas, Error
    assert!(matches!(events[0], StreamEvent::Start { .. }));
    assert!(matches!(
        events[3],
        StreamEvent::Error {
            error: crate::protocol::StreamError {
                kind: ErrorKind::UpstreamServer,
                ..
            }
        }
    ));

    let validator = validate(&events);
    validator
        .ensure_terminated()
        .expect("error terminates the stream");
    assert_eq!(validator.state(), StreamState::Failed);
    assert_eq!(provider.in_flight_requests(), 0);
}

#[tokio::test]
async fn stream_interruption_truncates_without_a_terminator() {
    let config = MockConfig::default()
        .with_reply(MockReply::fixed("abcdefgh"))
        .with_chunk_size(2)
        .with_fault(FaultInjection::new(FaultMode::StreamInterrupted).after_events(3));
    let provider = MockProvider::new(config.clone());
    let events = collect(&provider, request("mock-chat", "hi")).await;

    assert_eq!(events.len(), 3); // Start + 2 deltas, then the stream just stops
    assert!(matches!(events[0], StreamEvent::Start { .. }));
    assert_eq!(text_of(&events), "abcd");
    assert!(events.iter().all(|event| !event.is_terminal()));

    let validator = validate(&events);
    assert!(
        validator.ensure_terminated().is_err(),
        "a truncated stream must not validate as terminated"
    );
    assert_eq!(validator.state(), StreamState::Streaming);
    assert_eq!(provider.in_flight_requests(), 0);

    // Non-streaming calls have no truncation representation and surface it as
    // an upstream failure instead.
    let err = chat_error(config).await;
    assert_eq!(err.kind, ErrorKind::UpstreamServer);
    assert_eq!(err.upstream_status, Some(500));
}

// ---------------------------------------------------------------------------
// Cancellation and isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_a_stream_stops_generation_and_releases_the_registration() {
    let provider = MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::fixed("0123456789abcdef"))
            .with_chunk_size(2)
            .with_event_delay(Duration::from_millis(2)),
    );

    let mut stream = provider
        .chat_stream(&credential(), request_with_id("hi", "cancel-me"))
        .await
        .expect("stream starts");
    let survivor = provider
        .chat_stream(&credential(), request_with_id("hi", "survivor"))
        .await
        .expect("stream starts");

    let first = stream.next().await.expect("item").expect("event");
    assert!(matches!(first, StreamEvent::Start { .. }));
    assert_eq!(provider.in_flight_requests(), 2);

    provider
        .cancel(CancelRequest::new("cancel-me"))
        .await
        .expect("cancel in-flight request");

    let next = stream.next().await.expect("item").expect("event");
    match next {
        StreamEvent::Error { error } => assert_eq!(error.kind, ErrorKind::Cancelled),
        other => panic!("expected a cancellation error, got {other:?}"),
    }
    assert!(
        stream.next().await.is_none(),
        "no events may follow cancellation"
    );
    assert_eq!(provider.in_flight_requests(), 1);

    // The second request is independent and still completes normally.
    let events = tokio::time::timeout(Duration::from_secs(5), drain(survivor))
        .await
        .expect("survivor must terminate");
    validate(&events)
        .ensure_terminated()
        .expect("survivor terminated");
    assert_eq!(text_of(&events), "0123456789abcdef");
    assert_eq!(provider.in_flight_requests(), 0);

    // The registration is gone once the stream ended.
    assert!(provider
        .cancel(CancelRequest::new("cancel-me"))
        .await
        .is_err());
}

#[tokio::test]
async fn concurrent_streams_do_not_interfere() {
    let provider = Arc::new(MockProvider::new(
        MockConfig::default()
            .with_reply(MockReply::echo("echo: "))
            .with_chunk_size(3)
            .with_event_delay(Duration::from_millis(1)),
    ));

    let first = {
        let provider = Arc::clone(&provider);
        async move { collect(&provider, request("mock-chat", "alpha")).await }
    };
    let second = {
        let provider = Arc::clone(&provider);
        async move { collect(&provider, request("mock-chat", "beta")).await }
    };
    let (alpha, beta) = tokio::join!(first, second);

    assert_eq!(text_of(&alpha), "echo: alpha");
    assert_eq!(text_of(&beta), "echo: beta");
    for events in [&alpha, &beta] {
        validate(events).ensure_terminated().expect("terminated");
    }
    assert_eq!(provider.in_flight_requests(), 0);
}
