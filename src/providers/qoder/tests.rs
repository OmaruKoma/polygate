//! Offline tests for the Qoder protocol foundation.
//!
//! No test opens a socket. Fixtures are sanitized protocol frames, not captures
//! containing credentials.

use serde_json::{json, Value};

use crate::config::Region;
use crate::error::ErrorKind;
use crate::protocol::{
    ChatMessage, ChatRequest, ContentBlock, FinishReason, MessageRole, StreamError, StreamEvent,
    ToolCall, ToolChoice, ToolDefinition, ToolResult,
};

use super::{convert, cosy, error, events, region};

mod inference;

const CHAT_SSE: &str = include_str!("fixtures/chat_sse.txt");

fn identity() -> cosy::CosyIdentity {
    cosy::CosyIdentity::new("u9", "tok").with_name("N")
}

fn fixture_frames(chunk_size: usize) -> Vec<events::SseFrame> {
    let mut decoder = events::SseDecoder::new();
    let mut frames = Vec::new();
    for chunk in CHAT_SSE.as_bytes().chunks(chunk_size) {
        frames.extend(decoder.push(chunk).expect("fixture chunk"));
    }
    frames.extend(decoder.finish().expect("fixture finish"));
    frames
}

fn translated_fixture() -> Vec<StreamEvent> {
    let mut translator =
        events::QoderStreamTranslator::new("resp-1", Some("req-1".into()), "qmodel");
    let mut all = vec![translator.start_event()];
    for frame in fixture_frames(7) {
        all.extend(translator.translate_frame(&frame).expect("frame"));
    }
    all.extend(translator.end_of_stream());
    all
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

fn tool_args_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallArgsDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Region isolation
// ---------------------------------------------------------------------------

#[test]
fn cn_and_intl_realms_are_independent() {
    let cn = region::realm(Region::Cn);
    let intl = region::realm(Region::Intl);

    assert_eq!(cn.openapi_base, "https://openapi.qoder.com.cn");
    assert_eq!(intl.openapi_base, "https://openapi.qoder.sh");
    assert_eq!(cn.gateway_base, "https://gateway.qoder.com.cn");
    assert_eq!(intl.gateway_base, "https://api1.qoder.sh");
    assert_ne!(cn.website, intl.website);
    assert_ne!(cn.domain, intl.domain);
    assert_ne!(cn.user_agent, intl.user_agent);
    assert_eq!(cn.client_id, intl.client_id);
    assert!(!cn.send_redirect_uri && !intl.send_redirect_uri);

    assert_eq!(
        cn.gateway_candidates(),
        vec!["https://gateway.qoder.com.cn"]
    );
    assert_eq!(
        intl.gateway_candidates(),
        vec![
            "https://api1.qoder.sh",
            "https://api2.qoder.sh",
            "https://api3.qoder.sh"
        ]
    );
}

#[test]
fn nonce_shape_follows_the_realm() {
    let cn = region::realm(Region::Cn).new_nonce();
    let intl = region::realm(Region::Intl).new_nonce();
    assert_eq!(cn.len(), 36);
    assert!(cn.contains('-'));
    assert_eq!(intl.len(), 32);
    assert!(intl.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn urls_use_the_selected_realm_only() {
    let cn = region::realm(Region::Cn);
    let intl = region::realm(Region::Intl);
    assert_eq!(
        cn.chat_url(),
        "https://gateway.qoder.com.cn/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"
    );
    assert_eq!(
        intl.models_url(),
        "https://api1.qoder.sh/algo/api/v2/model/list?Encode=1"
    );
    assert_eq!(
        cn.quota_url(),
        "https://openapi.qoder.com.cn/api/v2/quota/usage"
    );
    assert_eq!(
        intl.device_poll_url(),
        "https://openapi.qoder.sh/api/v1/deviceToken/poll"
    );
    assert!(cn
        .campaign_claim_url("abc-1")
        .unwrap()
        .ends_with("/sash/api/v1/me/campaigns/abc-1/claim"));
    assert!(cn.campaign_claim_url("../x").is_err());
    assert!(cn.campaign_claim_url("").is_err());
}

// ---------------------------------------------------------------------------
// Chat body conversion
// ---------------------------------------------------------------------------

fn basic_request() -> ChatRequest {
    let mut request = ChatRequest::new(
        "qmodel",
        vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ],
    );
    request.max_output_tokens = Some(64);
    request.temperature = Some(0.2);
    request.top_p = Some(0.9);
    request.stop = vec!["STOP".into()];
    request.tools = vec![ToolDefinition::new(
        "get_weather",
        json!({"type": "object"}),
    )];
    request.tool_choice = Some(ToolChoice::Tool("get_weather".into()));
    request
}

#[test]
fn body_always_streams_and_keeps_verified_parameters() {
    let mut request = basic_request();
    request.stream = false;
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["agent_id"], json!("agent_common"));
    assert_eq!(body["model"], json!("qmodel"));
    assert_eq!(body["temperature"], json!(0.2));
    assert_eq!(body["top_p"], json!(0.9));
    assert_eq!(body["stop"], json!(["STOP"]));
    assert_eq!(body["parameters"]["max_tokens"], json!(64));
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["tools"][0]["type"], json!("function"));
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["type"],
        json!("object")
    );
    assert_eq!(
        body["tool_choice"]["function"]["name"],
        json!("get_weather")
    );
    assert_eq!(body["messages"][0]["role"], json!("system"));
    assert_eq!(body["messages"][1]["role"], json!("user"));
    assert_eq!(body["messages"][2]["role"], json!("assistant"));
}

#[test]
fn developer_role_is_normalised_to_system() {
    let request = ChatRequest::new(
        "qmodel",
        vec![ChatMessage::developer("mid-conversation instruction")],
    );
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["messages"][0]["role"], json!("system"));
}

#[test]
fn assistant_tool_history_is_openai_shaped_and_never_null_content() {
    let messages = vec![
        ChatMessage::user("weather?"),
        ChatMessage::new(
            MessageRole::Assistant,
            vec![ContentBlock::ToolCall(ToolCall::new(
                "call_1",
                "get_weather",
                r#"{"city":"Beijing"}"#,
            ))],
        ),
        ChatMessage::tool(ToolResult::new("call_1", "sunny")),
    ];
    let request = ChatRequest::new("qmodel", messages);
    let body = convert::build_body(&request).unwrap();
    let assistant = &body["messages"][1];
    assert_eq!(assistant["content"], json!(""));
    assert!(assistant["content"].is_string());
    assert_eq!(assistant["tool_calls"][0]["type"], json!("function"));
    assert_eq!(
        assistant["tool_calls"][0]["function"]["name"],
        json!("get_weather")
    );
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        json!(r#"{"city":"Beijing"}"#)
    );
    assert_eq!(body["messages"][2]["role"], json!("tool"));
    assert_eq!(body["messages"][2]["tool_call_id"], json!("call_1"));
    assert_eq!(body["messages"][2]["content"], json!("sunny"));
    assert!(body["messages"][2].get("is_error").is_none());
}

#[test]
fn orphan_tool_message_is_dropped() {
    let request = ChatRequest::new(
        "qmodel",
        vec![
            ChatMessage::user("hello"),
            ChatMessage::tool(ToolResult::new("call_missing", "ignored")),
        ],
    );
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn consecutive_assistants_union_their_tool_declarations() {
    let request = ChatRequest::new(
        "qmodel",
        vec![
            ChatMessage::user("two tools"),
            ChatMessage::new(
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new("call_a", "a", "{}"))],
            ),
            ChatMessage::new(
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new("call_b", "b", "{}"))],
            ),
            ChatMessage::tool(ToolResult::new("call_a", "A")),
            ChatMessage::tool(ToolResult::new("call_b", "B")),
        ],
    );
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 5);
    assert_eq!(body["messages"][3]["tool_call_id"], json!("call_a"));
    assert_eq!(body["messages"][4]["tool_call_id"], json!("call_b"));
}

#[test]
fn an_assistant_without_tool_calls_clears_the_declaration_set() {
    let request = ChatRequest::new(
        "qmodel",
        vec![
            ChatMessage::new(
                MessageRole::Assistant,
                vec![ContentBlock::ToolCall(ToolCall::new("call_a", "a", "{}"))],
            ),
            ChatMessage::assistant("done"),
            ChatMessage::tool(ToolResult::new("call_a", "A")),
        ],
    );
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn multiple_tool_results_become_separate_messages() {
    let request = ChatRequest::new(
        "qmodel",
        vec![
            ChatMessage::new(
                MessageRole::Assistant,
                vec![
                    ContentBlock::ToolCall(ToolCall::new("call_a", "a", "{}")),
                    ContentBlock::ToolCall(ToolCall::new("call_b", "b", "{}")),
                ],
            ),
            ChatMessage::new(
                MessageRole::Tool,
                vec![
                    ContentBlock::ToolResult(ToolResult::new("call_a", "A")),
                    ContentBlock::ToolResult(ToolResult::new("call_b", "B")),
                ],
            ),
        ],
    );
    let body = convert::build_body(&request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["tool_call_id"], json!("call_a"));
    assert_eq!(messages[2]["tool_call_id"], json!("call_b"));
}

#[test]
fn reasoning_is_preserved_on_assistant_history() {
    let request = ChatRequest::new(
        "qmodel",
        vec![ChatMessage::new(
            MessageRole::Assistant,
            vec![ContentBlock::reasoning("why"), ContentBlock::text("answer")],
        )],
    );
    let body = convert::build_body(&request).unwrap();
    assert_eq!(body["messages"][0]["content"], json!("answer"));
    assert_eq!(body["messages"][0]["reasoning_content"], json!("why"));
}

#[test]
fn unrepresentable_blocks_and_roles_are_rejected() {
    let request = ChatRequest::new(
        "qmodel",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::Unsupported {
                media_type: "image/png".into(),
                data: json!({}),
            }],
        )],
    );
    assert_eq!(
        convert::build_body(&request).unwrap_err().kind,
        ErrorKind::Unsupported
    );

    let request = ChatRequest::new(
        "qmodel",
        vec![ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::reasoning("not allowed")],
        )],
    );
    assert_eq!(
        convert::build_body(&request).unwrap_err().kind,
        ErrorKind::BadRequest
    );
}

// ---------------------------------------------------------------------------
// SSE frame decoding
// ---------------------------------------------------------------------------

#[test]
fn decoder_handles_split_frames_and_utf8_boundaries() {
    let frame = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"你好\\\"}}]}\"}\n\n";
    let bytes = frame.as_bytes();
    let mut decoder = events::SseDecoder::new();
    let mut frames = Vec::new();
    // One byte at a time splits even a multi-byte UTF-8 character.
    for byte in bytes {
        frames.extend(decoder.push(&[*byte]).unwrap());
    }
    frames.extend(decoder.finish().unwrap());
    assert_eq!(frames.len(), 1);
    let envelope = events::decode_envelope(&frames[0]).unwrap().unwrap();
    assert_eq!(envelope.status, 200);
    let chunk = events::parse_inner_chunk(&envelope.body).unwrap();
    assert_eq!(chunk.content.as_deref(), Some("你好"));
}

#[test]
fn decoder_handles_multiple_frames_crlf_and_comments() {
    let input = ": keepalive\r\n\r\ndata: [DONE]\r\n\r\ndata: {\"statusCodeValue\":200,\"body\":\"null\"}\r\n\r\n";
    let mut decoder = events::SseDecoder::new();
    let frames = decoder.push(input.as_bytes()).unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].data, "[DONE]");
    assert_eq!(
        frames[1].data,
        "{\"statusCodeValue\":200,\"body\":\"null\"}"
    );
}

#[test]
fn decoder_flushes_a_final_line_without_newline() {
    let mut decoder = events::SseDecoder::new();
    assert!(decoder.push(b"data: [DONE]").unwrap().is_empty());
    let frames = decoder.finish().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].data, "[DONE]");
}

#[test]
fn envelope_done_keepalive_and_status_are_decoded() {
    let done = events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "[DONE]".into(),
    })
    .unwrap()
    .unwrap();
    assert_eq!(done.body, "[DONE]");

    let keepalive = events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"null\"}".into(),
    })
    .unwrap()
    .unwrap();
    assert!(events::is_keepalive_body(&keepalive.body));

    let failed = events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":\"403\",\"body\":\"{\\\"code\\\":\\\"10605\\\",\\\"isQueued\\\":true}\"}"
            .into(),
    })
    .unwrap()
    .unwrap();
    assert_eq!(failed.status, 403);
    assert_eq!(
        error::classify_envelope(failed.status, &failed.body).kind,
        ErrorKind::RateLimited
    );

    // The official clients skip telemetry frames that are not JSON.
    assert!(events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "[NOTIFICATIONS]#{\"type\":\"x\"}".into(),
    })
    .unwrap()
    .is_none());

    // Quota sentinels are not JSON either; the exceeded one is a quota state,
    // the not-exceeded one is a keepalive.
    assert!(events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "[NOT_EXCEED_QUOTA]".into(),
    })
    .unwrap()
    .is_none());
    let exceeded = events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "[EXCEED_QUOTA]".into(),
    })
    .unwrap()
    .unwrap();
    assert_eq!(exceeded.status, 402);
    assert_eq!(
        error::classify_envelope(exceeded.status, &exceeded.body).kind,
        ErrorKind::QuotaExhausted
    );

    assert!(events::decode_envelope(&events::SseFrame {
        event: String::new(),
        data: "not-json".into(),
    })
    .is_err());
}

#[test]
fn quota_sentinels_end_the_stream_as_a_quota_error() {
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let err = translator
        .translate_frame(&events::SseFrame {
            event: String::new(),
            data: "[EXCEED_QUOTA]".into(),
        })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::QuotaExhausted);
    assert!(translator.is_finished());

    // Wrapped inside an envelope's body, both sentinels are also recognised.
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    assert!(translator
        .translate_frame(&events::SseFrame {
            event: String::new(),
            data: "{\"statusCodeValue\":200,\"body\":\"[NOT_EXCEED_QUOTA]\"}".into(),
        })
        .unwrap()
        .is_empty());
    let err = translator
        .translate_frame(&events::SseFrame {
            event: String::new(),
            data: "{\"statusCodeValue\":200,\"body\":\"[EXCEED_QUOTA] credits\"}".into(),
        })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::QuotaExhausted);
}

#[test]
fn inner_chunk_keeps_incomplete_arguments_raw() {
    let body = r#"{"choices":[{"delta":{"content":"hi","reasoning_content":"r","tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3},"model":"qmodel"}"#;
    let chunk = events::parse_inner_chunk(body).unwrap();
    assert_eq!(chunk.content.as_deref(), Some("hi"));
    assert_eq!(chunk.reasoning.as_deref(), Some("r"));
    assert_eq!(chunk.model.as_deref(), Some("qmodel"));
    assert_eq!(chunk.tool_calls.len(), 1);
    assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("get_weather"));
    assert_eq!(
        chunk.tool_calls[0].arguments.as_deref(),
        Some(r#"{"city":"#)
    );
    let usage = chunk.usage.unwrap();
    assert_eq!(usage.input_tokens, 1);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(usage.total_tokens, Some(3));
}

#[test]
fn legacy_function_call_is_normalised() {
    let body =
        r#"{"choices":[{"delta":{"function_call":{"name":"get_weather","arguments":"{}"}}}]}"#;
    let chunk = events::parse_inner_chunk(body).unwrap();
    assert_eq!(chunk.tool_calls.len(), 1);
    assert_eq!(chunk.tool_calls[0].index, None);
    assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("get_weather"));
}

#[test]
fn unindexed_tool_calls_use_their_array_position() {
    let body = r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"a"}},{"id":"b","function":{"name":"b"}}]}}]}"#;
    let chunk = events::parse_inner_chunk(body).unwrap();
    assert_eq!(chunk.tool_calls.len(), 2);
    assert_eq!(chunk.tool_calls[0].index, Some(0));
    assert_eq!(chunk.tool_calls[1].index, Some(1));
}

#[test]
fn blank_tool_call_fragments_are_ignored() {
    let body = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":""}}]}}]}"#;
    let chunk = events::parse_inner_chunk(body).unwrap();
    assert!(chunk.tool_calls.is_empty());
}

#[test]
fn embedded_payload_error_is_not_a_content_chunk() {
    let body = r#"{"error":{"code":401,"message":"expired"}}"#;
    assert_eq!(
        events::parse_inner_chunk(body).unwrap_err().kind,
        ErrorKind::AuthInvalid
    );
}

// ---------------------------------------------------------------------------
// Stream translator and aggregation
// ---------------------------------------------------------------------------

#[test]
fn fixture_stream_translates_to_text_reasoning_tool_usage_and_finish() {
    let all = translated_fixture();
    assert_eq!(text_of(&all), "Hello ");
    assert_eq!(reasoning_of(&all), "think ");
    assert_eq!(tool_args_of(&all), r#"{"city":"Beijing"}"#);

    let starts: Vec<_> = all
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCallStart { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![("call_1", "get_weather")]);

    let usage = all.iter().find_map(|event| match event {
        StreamEvent::Usage { usage } => Some(*usage),
        _ => None,
    });
    assert_eq!(usage.unwrap().total_tokens, Some(15));

    let finish = all.iter().find_map(|event| match event {
        StreamEvent::Finish { reason } => Some(reason.clone()),
        _ => None,
    });
    assert_eq!(finish, Some(FinishReason::ToolCalls));
    assert!(!all
        .iter()
        .any(|event| matches!(event, StreamEvent::Error { .. })));
}

#[test]
fn eof_without_done_or_finish_reason_is_an_error() {
    let frame = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"partial\\\"}}]}\"}"
            .into(),
    };
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let events = translator.translate_frame(&frame).unwrap();
    assert_eq!(text_of(&events), "partial");
    let end = translator.end_of_stream();
    assert_eq!(end.len(), 1);
    match &end[0] {
        StreamEvent::Error { error } => {
            assert_eq!(error.kind, ErrorKind::UpstreamServer);
            assert!(error.message.contains("ended before"));
        }
        other => panic!("expected a terminal error, got {other:?}"),
    }
}

#[test]
fn done_without_finish_reason_finishes_as_stop() {
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let frame = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"[DONE]\"}".into(),
    };
    let events = translator.translate_frame(&frame).unwrap();
    assert_eq!(
        events.last(),
        Some(&StreamEvent::finish(FinishReason::Stop))
    );
    assert!(translator.is_finished());
    assert!(translator.end_of_stream().is_empty());
}

#[test]
fn keepalive_frames_are_ignored() {
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let frame = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"null\"}".into(),
    };
    assert!(translator.translate_frame(&frame).unwrap().is_empty());
}

#[test]
fn arguments_before_the_name_are_buffered() {
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let first = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"tool_calls\\\":[{\\\"index\\\":0,\\\"id\\\":\\\"call_1\\\",\\\"function\\\":{\\\"arguments\\\":\\\"{\\\\\\\"a\\\\\\\":\\\"}}]}}]}\"}"
            .into(),
    };
    assert!(translator.translate_frame(&first).unwrap().is_empty());

    let second = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"tool_calls\\\":[{\\\"index\\\":0,\\\"function\\\":{\\\"name\\\":\\\"f\\\"}}]}}]}\"}"
            .into(),
    };
    let events = translator.translate_frame(&second).unwrap();
    assert!(matches!(
        events.first(),
        Some(StreamEvent::ToolCallStart { id, name, .. }) if id == "call_1" && name == "f"
    ));
    assert_eq!(tool_args_of(&events), r#"{"a":"#);
}

#[test]
fn translated_fixture_aggregates_into_a_response() {
    let all = translated_fixture();
    let response = convert::aggregate("resp-1", Some("req-1".into()), "qmodel", &all).unwrap();
    assert_eq!(response.text(), "Hello ");
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.usage.unwrap().total_tokens, Some(15));
    assert!(response
        .message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Reasoning { .. })));
    let calls: Vec<&ToolCall> = response.message.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].arguments, r#"{"city":"Beijing"}"#);
}

#[test]
fn aggregate_propagates_terminal_errors() {
    let events = vec![StreamEvent::error(StreamError::new(
        ErrorKind::BadRequest,
        "bad request",
    ))];
    let error = convert::aggregate("resp", None, "qmodel", &events).unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadRequest);
}

#[test]
fn translate_inner_error_status_returns_classified_error() {
    let mut translator = events::QoderStreamTranslator::new("resp", None, "qmodel");
    let frame = events::SseFrame {
        event: String::new(),
        data: "{\"statusCodeValue\":402,\"body\":\"{\\\"message\\\":\\\"balance\\\"}\"}".into(),
    };
    let error = translator.translate_frame(&frame).unwrap_err();
    assert_eq!(error.kind, ErrorKind::QuotaExhausted);
}

#[test]
fn signer_and_urls_work_without_network() {
    let session = cosy::CosySession::new(identity(), *b"0123456789abcdef").unwrap();
    let cn_url = region::realm(Region::Cn).chat_url();
    let intl_url = region::realm(Region::Intl).chat_url();
    let cn = session.sign("BODY", &cn_url, "1700000000", "rid").unwrap();
    let intl = session
        .sign("BODY", &intl_url, "1700000000", "rid")
        .unwrap();
    assert_eq!(cn.path, intl.path);
    assert!(cn.authorization.starts_with("Bearer COSY."));
    assert_eq!(cn.path, "/api/v2/service/pro/sse/agent_chat_generation");
}

#[test]
fn body_value_is_stable_json_for_signing() {
    let request = basic_request();
    let body = convert::build_body(&request).unwrap();
    let encoded = convert::encode_body(&body).unwrap();
    let raw = serde_json::to_string(&body).unwrap();
    assert!(!encoded.is_empty());
    assert_eq!(cosy::qoder_decode(&encoded).unwrap(), raw.as_bytes());
    assert!(body.is_object());
    let _: Value = body;
}

// ---------------------------------------------------------------------------
// M13-D2-C: quota probe and auth probe (mock server, no real upstream)
// ---------------------------------------------------------------------------

mod openapi {
    use super::*;
    use crate::accounts::{AuthStatus, Credential};
    use crate::config::{Region, Verification};
    use crate::providers::{Provider, ProviderClass};
    use crate::quota::QuotaStatus;
    use crate::test_support::{spawn_server, Reply};

    fn provider(base: &str) -> super::super::provider::QoderProvider {
        super::super::provider::QoderProvider::with_base(
            "qoder-cn",
            Region::Cn,
            Verification::Experimental,
            reqwest::Client::new(),
            base,
        )
    }

    fn credential(expires_at: i64) -> Credential {
        Credential::from_value(json!({
            "access_token": "dt-access",
            "refresh_token": "drt-refresh",
            "uid": "uid-1",
            "nickname": "N",
            "realm": "cn",
            "expires_at": expires_at,
        }))
    }

    #[tokio::test]
    async fn quota_probe_uses_bearer_and_reference_headers() {
        let server = spawn_server(|request| match request.path() {
            "/api/v2/quota/usage" => Reply::json(
                200,
                json!({
                    "userQuota": {"total": 100, "remaining": 40},
                    "addOnQuota": {"total": 20, "remaining": 20}
                })
                .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let snapshot = provider(&server.base_url)
            .quota(&credential(0))
            .await
            .unwrap();
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.remaining, Some(60.0));
        assert_eq!(snapshot.total, Some(120.0));

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].header("authorization"),
            Some("Bearer dt-access")
        );
        assert_eq!(
            requests[0].header("x-machine-id"),
            Some(cosy::derive_id("uid-1", "machine").as_str())
        );
        assert_eq!(requests[0].header("origin"), Some("https://qoder.cn"));
        assert!(requests[0].header("x-request-id").is_some());
    }

    #[tokio::test]
    async fn quota_rejection_keeps_unknown_semantics() {
        let server =
            spawn_server(|_| Reply::json(402, r#"{"message":"Billing daily count exceeded"}"#))
                .await;
        let error = provider(&server.base_url)
            .quota(&credential(0))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::QuotaExhausted);
    }

    #[tokio::test]
    async fn auth_status_probes_userinfo() {
        let server = spawn_server(|request| match request.path() {
            "/api/v1/userinfo" => Reply::json(200, json!({"id": "uid-1"}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let status = provider(&server.base_url)
            .auth_status(&credential(0))
            .await
            .unwrap();
        assert_eq!(status, AuthStatus::Valid);
    }

    #[tokio::test]
    async fn expired_local_expiry_skips_the_network() {
        let server = spawn_server(|_| Reply::json(500, "{}")).await;
        let status = provider(&server.base_url)
            .auth_status(&credential(1))
            .await
            .unwrap();
        assert_eq!(status, AuthStatus::Expired);
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn provider_declares_every_implemented_capability() {
        let provider = provider("http://127.0.0.1:9");
        let caps = provider.capabilities();
        assert!(caps.authentication);
        assert!(caps.quota);
        assert!(caps.model_discovery);
        assert!(caps.chat);
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(!caps.native_session);
        assert!(!caps.agent_task);
        assert!(!caps.cancellation);
        assert_eq!(caps.class(), Some(ProviderClass::Model));
        assert_eq!(provider.info().kind, crate::config::ProviderKind::Qoder);
    }

    #[test]
    fn stored_credential_requires_token_and_uid() {
        let missing_uid = Credential::from_value(json!({"access_token": "dt-a"}));
        let error =
            super::super::credential::QoderCredential::from_credential(&missing_uid, Region::Cn)
                .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);

        let stored = Credential::from_value(json!({
            "access_token": "dt-a",
            "uid": "u-1",
            "realm": "intl",
            "expires_at": -5
        }));
        let credential =
            super::super::credential::QoderCredential::from_credential(&stored, Region::Cn)
                .unwrap();
        assert_eq!(
            credential.realm,
            Region::Cn,
            "realm comes from the account row"
        );
        assert_eq!(credential.expires_at, 0);
        assert_eq!(
            credential.user_type,
            super::super::credential::DEFAULT_USER_TYPE
        );
    }
}
