use std::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::*;
use crate::error::{ErrorKind, GatewayError};

fn roundtrip<T>(value: &T) -> T
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, value, "roundtrip changed the value: {json}");
    back
}

fn start() -> StreamEvent {
    StreamEvent::start("resp_1", Some("req_1".into()), "mock-chat")
}

fn usage() -> Usage {
    Usage {
        reasoning_tokens: Some(4),
        cached_input_tokens: Some(2),
        total_tokens: Some(30),
        ..Usage::new(10, 20)
    }
}

fn feed(validator: &mut StreamEventValidator, events: &[StreamEvent]) {
    for (index, event) in events.iter().enumerate() {
        validator
            .validate(event)
            .unwrap_or_else(|e| panic!("event {index} unexpectedly rejected: {e}"));
    }
}

fn reject(validator: &mut StreamEventValidator, event: &StreamEvent) -> GatewayError {
    match validator.validate(event) {
        Ok(()) => panic!("event should have been rejected: {event:?}"),
        Err(e) => e,
    }
}

fn assert_violation(err: &GatewayError) {
    assert_eq!(err.kind, ErrorKind::UpstreamServer);
    assert!(
        err.message.starts_with("invalid stream event sequence:"),
        "unexpected message: {}",
        err.message
    );
}

fn full_request() -> ChatRequest {
    let mut request = ChatRequest::new(
        "mock-chat",
        vec![
            ChatMessage::system("you are a coding agent"),
            ChatMessage::user("list the files"),
            ChatMessage::new(
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new(
                    "call_1",
                    "list_dir",
                    r#"{"path":"."}"#,
                ))],
            ),
            ChatMessage::tool(ToolResult::new("call_1", "a.txt\nb.txt")),
            ChatMessage::user("summarise a.txt"),
        ],
    );
    request.request_id = Some("req_42".into());
    request.tools = vec![ToolDefinition::new(
        "list_dir",
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
    .with_description("list a directory")];
    request.tool_choice = Some(ToolChoice::Auto);
    request.max_output_tokens = Some(1024);
    request.temperature = Some(0.2);
    request.top_p = Some(0.9);
    request.stop = vec!["END".into()];
    request.stream = true;
    request.metadata = Some(serde_json::json!({"client": "test"}));
    request
}

fn full_response() -> ChatResponse {
    let mut response = ChatResponse::new(
        "resp_42",
        "mock-chat",
        ChatMessage::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::reasoning("checked the directory"),
                ContentBlock::text("a.txt has three lines."),
                ContentBlock::ToolCall(ToolCall::new("call_9", "read_file", r#"{"path":"a.txt"}"#)),
            ],
        ),
        FinishReason::ToolCalls,
    );
    response.request_id = Some("req_42".into());
    response.usage = Some(usage());
    response
}

#[test]
fn chat_request_roundtrips() {
    let request = full_request();
    roundtrip(&request);

    let json = serde_json::to_value(&request).expect("serialize");
    assert_eq!(json["model"], "mock-chat");
    assert_eq!(json["messages"].as_array().unwrap().len(), 5);
    assert_eq!(json["tool_choice"], "auto");
    assert_eq!(json["tools"][0]["name"], "list_dir");
    assert_eq!(json["stream"], true);
}

#[test]
fn minimal_request_omits_absent_optional_fields() {
    let request = ChatRequest::new("mock-chat", vec![ChatMessage::user("hi")]);
    let json = serde_json::to_value(&request).expect("serialize");
    for absent in [
        "request_id",
        "tools",
        "tool_choice",
        "max_output_tokens",
        "temperature",
        "top_p",
        "stop",
        "metadata",
    ] {
        assert!(json.get(absent).is_none(), "{absent} should be omitted");
    }
    assert_eq!(json["stream"], false);
    roundtrip(&request);
}

#[test]
fn chat_response_roundtrips() {
    let response = full_response();
    roundtrip(&response);

    let json = serde_json::to_value(&response).expect("serialize");
    assert_eq!(json["id"], "resp_42");
    assert_eq!(json["finish_reason"], "tool_calls");
    assert_eq!(json["usage"]["input_tokens"], 10);
    assert_eq!(json["usage"]["total_tokens"], 30);
    assert_eq!(json["message"]["content"][1]["type"], "text");
    assert_eq!(response.text(), "a.txt has three lines.");
    assert_eq!(response.usage.unwrap().total(), 30);
}

#[test]
fn minimal_response_omits_absent_optional_fields() {
    let response = ChatResponse::new(
        "resp_1",
        "mock-chat",
        ChatMessage::assistant("ok"),
        FinishReason::Stop,
    );
    let json = serde_json::to_value(&response).expect("serialize");
    assert!(json.get("request_id").is_none());
    assert!(json.get("usage").is_none());
    roundtrip(&response);
}

#[test]
fn multi_turn_conversation_preserves_roles_and_content() {
    let messages = vec![
        ChatMessage::system("instructions"),
        ChatMessage::developer("be concise"),
        ChatMessage::user("hello"),
        ChatMessage::new(
            MessageRole::Assistant,
            vec![
                ContentBlock::reasoning("greet back"),
                ContentBlock::text("hi"),
                ContentBlock::ToolCall(ToolCall::new("call_1", "lookup", "{}")),
            ],
        ),
        ChatMessage::new(
            MessageRole::Tool,
            vec![
                ContentBlock::ToolResult(ToolResult::new("call_1", "found")),
                ContentBlock::Unsupported {
                    media_type: "image/png".into(),
                    data: serde_json::json!({"width": 10}),
                },
            ],
        ),
    ];
    let request = ChatRequest::new("mock-chat", messages);
    let back = roundtrip(&request);

    let roles: Vec<_> = back.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            MessageRole::System,
            MessageRole::Developer,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::Tool,
        ]
    );
    assert!(MessageRole::System.is_instruction());
    assert!(MessageRole::Developer.is_instruction());
    assert!(!MessageRole::User.is_instruction());

    let assistant = &back.messages[3];
    assert_eq!(assistant.text_content(), "hi");
    assert_eq!(assistant.tool_calls().count(), 1);
    assert_eq!(
        assistant.tool_calls().next().unwrap().name,
        "lookup",
        "tool call survives multi-turn roundtrip"
    );

    let tool = &back.messages[4];
    assert_eq!(tool.tool_results().next().unwrap().content, "found");
    assert_eq!(tool.unsupported_blocks().count(), 1);
    assert!(matches!(
        tool.unsupported_blocks().next().unwrap(),
        ContentBlock::Unsupported { media_type, .. } if media_type == "image/png"
    ));
}

#[test]
fn tool_call_and_result_roundtrip() {
    let call = ToolCall::new("call_7", "read_file", r#"{"path":"a.txt","lines":3}"#);
    let back = roundtrip(&call);
    assert_eq!(back.id, "call_7");
    assert_eq!(
        back.parse_arguments().expect("valid json"),
        serde_json::json!({"path": "a.txt", "lines": 3})
    );

    let empty = ToolCall::new("call_8", "now", "");
    assert_eq!(
        empty.parse_arguments().expect("empty args"),
        serde_json::json!({})
    );

    let invalid = ToolCall::new("call_9", "read_file", r#"{"path":"#);
    let err = invalid.parse_arguments().expect_err("invalid json");
    assert_eq!(err.kind, ErrorKind::UpstreamServer);
    assert!(err.message.contains("call_9"));
    assert!(!err.message.contains(r#"{"path"#), "raw args must not leak");

    let result = ToolResult::error("call_7", "permission denied");
    let back = roundtrip(&result);
    assert_eq!(back.tool_call_id, "call_7");
    assert!(back.is_error);
    assert_eq!(back.content, "permission denied");
}

#[test]
fn valid_stream_start_text_finish() {
    let mut validator = StreamEventValidator::new();
    assert_eq!(validator.state(), StreamState::Fresh);

    feed(
        &mut validator,
        &[
            start(),
            StreamEvent::text_delta("hello "),
            StreamEvent::text_delta("world"),
            StreamEvent::usage(Usage::new(5, 2)),
            StreamEvent::finish(FinishReason::Stop),
        ],
    );
    assert_eq!(validator.state(), StreamState::Finished);
    assert!(validator.is_terminated());
    assert_eq!(validator.response_id(), Some("resp_1"));
    assert_eq!(validator.usage(), Some(Usage::new(5, 2)));
    validator.ensure_terminated().expect("stream is complete");
}

#[test]
fn args_delta_before_tool_call_start_is_rejected() {
    let mut validator = StreamEventValidator::new();
    let err = reject(
        &mut validator,
        &StreamEvent::tool_call_args_delta("call_1", "{"),
    );
    assert_violation(&err);
    assert_eq!(validator.state(), StreamState::Fresh);

    feed(&mut validator, &[start()]);
    let err = reject(
        &mut validator,
        &StreamEvent::tool_call_args_delta("call_1", "{"),
    );
    assert_violation(&err);
    assert!(err.message.contains("unknown id `call_1`"));
    assert_eq!(
        validator.state(),
        StreamState::Streaming,
        "a rejected event must not advance the validator"
    );
}

#[test]
fn events_after_finish_are_rejected() {
    let mut validator = StreamEventValidator::new();
    feed(&mut validator, &[start(), StreamEvent::text_delta("done")]);
    // Duplicate finish while still streaming is checked separately.
    feed(&mut validator, &[StreamEvent::finish(FinishReason::Stop)]);
    assert_eq!(validator.state(), StreamState::Finished);

    for event in [
        StreamEvent::text_delta("late"),
        StreamEvent::reasoning_delta("late"),
        StreamEvent::tool_call_start("late", "tool"),
        StreamEvent::usage(Usage::new(1, 1)),
        StreamEvent::finish(FinishReason::Stop),
        StreamEvent::error(StreamError::new(ErrorKind::Timeout, "late")),
        start(),
    ] {
        let err = reject(&mut validator, &event);
        assert_violation(&err);
        assert!(err.message.contains("finished"));
    }
    validator.ensure_terminated().expect("stream is complete");
}

#[test]
fn events_after_error_are_rejected() {
    let mut validator = StreamEventValidator::new();
    feed(
        &mut validator,
        &[
            start(),
            StreamEvent::error(StreamError::new(ErrorKind::UpstreamServer, "boom")),
        ],
    );
    assert_eq!(validator.state(), StreamState::Failed);

    for event in [
        StreamEvent::text_delta("late"),
        StreamEvent::finish(FinishReason::Stop),
        StreamEvent::error(StreamError::new(ErrorKind::Timeout, "again")),
        StreamEvent::usage(Usage::new(1, 1)),
    ] {
        let err = reject(&mut validator, &event);
        assert_violation(&err);
        assert!(err.message.contains("failed"));
    }
    validator.ensure_terminated().expect("stream is terminated");
}

#[test]
fn error_before_start_is_allowed_as_immediate_failure() {
    let mut validator = StreamEventValidator::new();
    feed(
        &mut validator,
        &[StreamEvent::error(StreamError::new(
            ErrorKind::Transport,
            "connect failed",
        ))],
    );
    assert_eq!(validator.state(), StreamState::Failed);
    assert!(reject(&mut validator, &start()).message.contains("failed"));
}

#[test]
fn interleaved_tool_calls_keep_fragments_by_id() {
    let events = vec![
        start(),
        StreamEvent::tool_call_start("call_a", "read_file"),
        StreamEvent::tool_call_start("call_b", "list_dir"),
        StreamEvent::tool_call_args_delta("call_a", r#"{"path":"#),
        StreamEvent::tool_call_args_delta("call_b", r#"{"path":"#),
        StreamEvent::tool_call_args_delta("call_a", r#""a.txt"}"#),
        StreamEvent::tool_call_args_delta("call_b", r#""."}"#),
        StreamEvent::usage(Usage::new(12, 3)),
        StreamEvent::finish(FinishReason::ToolCalls),
    ];

    let mut validator = StreamEventValidator::new();
    feed(&mut validator, &events);
    validator.ensure_terminated().expect("complete");

    let mut fragments: std::collections::HashMap<&str, String> = Default::default();
    for event in &events {
        if let StreamEvent::ToolCallArgsDelta { id, delta } = event {
            fragments.entry(id.as_str()).or_default().push_str(delta);
        }
    }
    assert_eq!(fragments.len(), 2);
    let args_a: serde_json::Value =
        serde_json::from_str(&fragments["call_a"]).expect("call_a json");
    let args_b: serde_json::Value =
        serde_json::from_str(&fragments["call_b"]).expect("call_b json");
    assert_eq!(args_a, serde_json::json!({"path": "a.txt"}));
    assert_eq!(args_b, serde_json::json!({"path": "."}));
}

#[test]
fn duplicate_tool_call_id_is_rejected() {
    let mut validator = StreamEventValidator::new();
    feed(
        &mut validator,
        &[
            start(),
            StreamEvent::tool_call_start("call_1", "read_file"),
            StreamEvent::tool_call_args_delta("call_1", "{}"),
        ],
    );
    let err = reject(
        &mut validator,
        &StreamEvent::tool_call_start("call_1", "list_dir"),
    );
    assert_violation(&err);
    assert!(err.message.contains("already used"));
    assert_eq!(validator.state(), StreamState::Streaming);

    let err = reject(&mut validator, &StreamEvent::tool_call_start("  ", "x"));
    assert_violation(&err);
    assert!(err.message.contains("non-empty id"));
}

#[test]
fn duplicate_finish_and_finish_before_start_are_rejected() {
    let mut validator = StreamEventValidator::new();
    let err = reject(&mut validator, &StreamEvent::finish(FinishReason::Stop));
    assert_violation(&err);
    assert!(err.message.contains("before start"));

    feed(
        &mut validator,
        &[start(), StreamEvent::finish(FinishReason::Stop)],
    );
    let err = reject(&mut validator, &StreamEvent::finish(FinishReason::Length));
    assert_violation(&err);
    assert!(err.message.contains("finished"));
}

#[test]
fn usage_ordering_is_enforced() {
    let mut validator = StreamEventValidator::new();
    let err = reject(&mut validator, &StreamEvent::usage(Usage::new(1, 1)));
    assert_violation(&err);
    assert!(err.message.contains("before start"));

    feed(&mut validator, &[start(), StreamEvent::text_delta("x")]);
    feed(&mut validator, &[StreamEvent::usage(Usage::new(3, 4))]);
    assert_eq!(validator.state(), StreamState::AfterUsage);

    let err = reject(&mut validator, &StreamEvent::usage(Usage::new(5, 6)));
    assert_violation(&err);
    assert!(err.message.contains("after usage"));

    for event in [
        StreamEvent::text_delta("late"),
        StreamEvent::tool_call_start("call_1", "tool"),
        StreamEvent::tool_call_args_delta("call_1", "{}"),
    ] {
        let err = reject(&mut validator, &event);
        assert_violation(&err);
        assert!(err.message.contains("after usage"));
    }

    feed(&mut validator, &[StreamEvent::finish(FinishReason::Stop)]);
    assert_eq!(validator.state(), StreamState::Finished);
    assert_eq!(validator.usage(), Some(Usage::new(3, 4)));
}

#[test]
fn duplicate_start_is_rejected() {
    let mut validator = StreamEventValidator::new();
    feed(&mut validator, &[start()]);
    let err = reject(&mut validator, &start());
    assert_violation(&err);
    assert!(err.message.contains("duplicate start"));
    assert_eq!(validator.state(), StreamState::Streaming);

    let mut validator = StreamEventValidator::new();
    let err = reject(&mut validator, &StreamEvent::start("", None, "mock-chat"));
    assert_violation(&err);
    assert!(err.message.contains("non-empty response id"));
}

#[test]
fn stream_events_roundtrip_through_json() {
    let events = vec![
        start(),
        StreamEvent::text_delta("hello"),
        StreamEvent::reasoning_delta("thinking"),
        StreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "read_file".into(),
            index: Some(0),
        },
        StreamEvent::tool_call_args_delta("call_1", r#"{"path":"#),
        StreamEvent::usage(usage()),
        StreamEvent::finish(FinishReason::Length),
        StreamEvent::error(StreamError::new(ErrorKind::RateLimited, "slow down")),
    ];
    for event in &events {
        roundtrip(event);
    }

    let tags: Vec<String> = events
        .iter()
        .map(|e| {
            serde_json::to_value(e).expect("serialize")["event"]
                .as_str()
                .expect("event tag")
                .to_string()
        })
        .collect();
    assert_eq!(
        tags,
        vec![
            "start",
            "text_delta",
            "reasoning_delta",
            "tool_call_start",
            "tool_call_args_delta",
            "usage",
            "finish",
            "error",
        ]
    );

    let start_json = serde_json::to_value(&events[0]).expect("serialize");
    assert_eq!(start_json["id"], "resp_1");
    assert_eq!(start_json["model"], "mock-chat");
    let delta_json = serde_json::to_value(&events[3]).expect("serialize");
    assert_eq!(delta_json["index"], 0);
}

#[test]
fn unknown_fields_are_ignored() {
    let raw = r#"{
        "model": "mock-chat",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}], "future": 1}],
        "unknown_request_field": true
    }"#;
    let request: ChatRequest = serde_json::from_str(raw).expect("unknown fields ignored");
    assert_eq!(request.model, "mock-chat");
    assert_eq!(request.messages.len(), 1);

    let raw_event = r#"{"event":"text_delta","text":"hi","future":true}"#;
    let event: StreamEvent = serde_json::from_str(raw_event).expect("unknown event field ignored");
    assert_eq!(event, StreamEvent::text_delta("hi"));

    let raw_response = r#"{"id":"r","model":"m","message":{"role":"assistant","content":[]},
        "finish_reason":"stop","created_at":"2026-01-01T00:00:00Z","extra":1}"#;
    let response: ChatResponse =
        serde_json::from_str(raw_response).expect("unknown response field ignored");
    assert_eq!(response.finish_reason, FinishReason::Stop);
}

#[test]
fn stream_end_without_terminator_is_rejected() {
    let validator = StreamEventValidator::new();
    let err = validator.ensure_terminated().expect_err("no events");
    assert_violation(&err);

    let mut validator = StreamEventValidator::new();
    feed(&mut validator, &[start()]);
    let err = validator.ensure_terminated().expect_err("no terminator");
    assert_violation(&err);
    assert!(err.message.contains("without finish or error"));

    let mut validator = StreamEventValidator::new();
    feed(
        &mut validator,
        &[start(), StreamEvent::usage(Usage::new(1, 1))],
    );
    assert!(validator.ensure_terminated().is_err());
}

#[test]
fn finish_reason_normalisation() {
    assert_eq!(FinishReason::from_upstream(""), FinishReason::Unknown);
    assert_eq!(FinishReason::from_upstream("   "), FinishReason::Unknown);
    for raw in ["stop", "end_turn", "stop_sequence"] {
        assert_eq!(
            FinishReason::from_upstream(raw),
            FinishReason::Stop,
            "{raw}"
        );
    }
    assert_eq!(FinishReason::from_upstream("length"), FinishReason::Length);
    assert_eq!(
        FinishReason::from_upstream("max_tokens"),
        FinishReason::Length
    );
    assert_eq!(
        FinishReason::from_upstream("tool_use"),
        FinishReason::ToolCalls
    );
    assert_eq!(
        FinishReason::from_upstream("safety"),
        FinishReason::ContentFilter
    );
    assert_eq!(
        FinishReason::from_upstream("vendor_specific"),
        FinishReason::Other("vendor_specific".into())
    );
    assert_eq!(
        FinishReason::from_upstream("vendor_specific").as_str(),
        "vendor_specific"
    );
    assert_eq!(FinishReason::Unknown.as_str(), "unknown");
    roundtrip(&FinishReason::from_upstream("vendor_specific"));
}

#[test]
fn stream_error_carries_no_diagnostic_detail() {
    let err = GatewayError::timeout("upstream timed out")
        .with_detail("Authorization: Bearer sk-live-secret")
        .retry_after(9);
    let stream_err = StreamError::from_gateway_error(&err);
    assert_eq!(stream_err.kind, ErrorKind::Timeout);
    assert_eq!(stream_err.message, "upstream timed out");
    assert_eq!(stream_err.retry_after, Some(9));

    let json = serde_json::to_string(&stream_err).expect("serialize");
    assert!(!json.contains("sk-live-secret"));
    assert!(!json.contains("detail"));

    let redacted = StreamError::new(ErrorKind::UpstreamServer, "call failed: token=abc123");
    assert!(!redacted.message.contains("abc123"));
    roundtrip(&redacted);
}

#[test]
fn usage_totals_fall_back_to_sum() {
    let usage = Usage::new(7, 5);
    assert_eq!(usage.total(), 12);
    assert_eq!(usage.total_tokens, None);

    let reported = Usage {
        total_tokens: Some(99),
        ..Usage::new(7, 5)
    };
    assert_eq!(reported.total(), 99);
    roundtrip(&reported);
}
