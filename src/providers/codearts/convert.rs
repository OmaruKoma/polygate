//! Internal protocol <-> CodeArts `/api/v2/chat/completions` conversion.
//!
//! The upstream v2 endpoint speaks the standard OpenAI chunk shape (evidence:
//! Coding2API `events.py`, captured 2026-09-30): `choices[].delta.content`,
//! `reasoning_content`, `tool_calls`, `finish_reason`, a final `usage` frame and
//! `data:[DONE]`. Errors are carried inside HTTP 200 frames as an
//! `error_code`/`error_msg` pair.
//!
//! Legacy `text` snapshot frames (cumulative text) are also understood and
//! converted to deltas, so an upstream rollback does not duplicate output.

use serde_json::{json, Map, Value};

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, Usage,
};

use super::error::classify_stream_error;
use crate::providers::{is_image_media, openai_content};

/// Upstream request body. `stream` is always true: non-streaming calls aggregate
/// the same stream locally.
pub fn build_body(request: &ChatRequest, chat_id: &str) -> Result<Value, GatewayError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert(
        "messages".into(),
        json!(messages_to_upstream(&request.messages)?),
    );
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("prompt_cache_key".into(), json!(chat_id));
    body.insert("tool_stream".into(), json!(true));

    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(max_tokens) = request.max_output_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
    }
    if !request.stop.is_empty() {
        body.insert("stop".into(), json!(request.stop));
    }
    // The upstream accepts both knobs as top-level fields (the reference
    // implementation sends them by default); forward the caller's explicit
    // values instead of inventing a default.
    if let Some(enable_thinking) = request.enable_thinking {
        body.insert("enable_thinking".into(), json!(enable_thinking));
    }
    if let Some(reasoning_effort) = &request.reasoning_effort {
        body.insert("reasoning_effort".into(), json!(reasoning_effort));
    }
    if !request.tools.is_empty() {
        body.insert("tools".into(), json!(tools_to_upstream(request)?));
    }
    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_upstream(choice));
    }
    Ok(Value::Object(body))
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    messages.iter().map(message_to_upstream).collect()
}

fn message_to_upstream(message: &ChatMessage) -> Result<Value, GatewayError> {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    match message.role {
        MessageRole::Tool => {
            let result = message.tool_results().next().ok_or_else(|| {
                GatewayError::bad_request("a `tool` message must carry a tool result")
            })?;
            if message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                )
            }) {
                return Err(GatewayError::new(
                    ErrorKind::Unsupported,
                    "a `tool` message cannot carry image content",
                ));
            }
            Ok(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": result.content,
            }))
        }
        role => {
            let tool_calls = message
                .tool_calls()
                .map(tool_call_to_upstream)
                .collect::<Vec<_>>();
            let content = if tool_calls.is_empty() {
                openai_content("CodeArts", &message.content)?
            } else {
                // Assistant turns with tool calls must send text (or null),
                // never an image part list.
                if message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                    )
                }) {
                    return Err(GatewayError::new(
                        ErrorKind::Unsupported,
                        "an assistant tool-call message cannot carry image content",
                    ));
                }
                if text.is_empty() {
                    Value::Null
                } else {
                    json!(text)
                }
            };
            let mut object = Map::new();
            object.insert("role".into(), json!(role_name(role)));
            if let Some(name) = &message.name {
                object.insert("name".into(), json!(name));
            }
            object.insert("content".into(), content);
            if !tool_calls.is_empty() {
                object.insert("tool_calls".into(), json!(tool_calls));
            }
            Ok(Value::Object(object))
        }
    }
}

fn tool_call_to_upstream(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.arguments,
        },
    })
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System | MessageRole::Developer => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn tools_to_upstream(request: &ChatRequest) -> Result<Vec<Value>, GatewayError> {
    request
        .tools
        .iter()
        .map(|tool| {
            let mut function = Map::new();
            function.insert("name".into(), json!(tool.name));
            if let Some(description) = &tool.description {
                function.insert("description".into(), json!(description));
            }
            function.insert(
                "parameters".into(),
                tool.parameters
                    .clone()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            Ok(json!({"type": "function", "function": Value::Object(function)}))
        })
        .collect()
}

fn tool_choice_to_upstream(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SseLine {
    Done,
    Payload(Value),
    Ignore,
}

/// One line of the upstream stream. Empty lines, event names, comments and
/// truncated heartbeat brackets carry no semantics.
pub fn parse_sse_line(line: &str) -> Result<SseLine, GatewayError> {
    let text = line.trim();
    if text.is_empty()
        || text.starts_with(':')
        || text.starts_with("event:")
        || text.starts_with("id:")
    {
        return Ok(SseLine::Ignore);
    }
    let payload = if let Some(rest) = text.strip_prefix("data:") {
        rest.trim()
    } else if text.starts_with('{') || text.starts_with('[') {
        text
    } else {
        return Ok(SseLine::Ignore);
    };
    if payload == "[DONE]" {
        return Ok(SseLine::Done);
    }
    if payload
        .chars()
        .all(|ch| ch.is_whitespace() || "{}[]:,".contains(ch))
    {
        return Ok(SseLine::Ignore);
    }
    let value: Value = serde_json::from_str(payload).map_err(|_| {
        GatewayError::upstream_server("CodeArts returned an unparsable stream frame")
    })?;
    Ok(SseLine::Payload(value))
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    started: bool,
    args: String,
}

/// Translate upstream frames into the internal stream vocabulary.
pub struct StreamTranslator {
    response_id: String,
    request_id: Option<String>,
    model: String,
    snapshot: String,
    tools: std::collections::BTreeMap<u64, ToolState>,
    usage: Option<Usage>,
    finish_reason: Option<FinishReason>,
    started: bool,
    finished: bool,
}

impl StreamTranslator {
    pub fn new(
        response_id: impl Into<String>,
        request_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            request_id,
            model: model.into(),
            snapshot: String::new(),
            tools: std::collections::BTreeMap::new(),
            usage: None,
            finish_reason: None,
            started: false,
            finished: false,
        }
    }

    pub fn start_event(&mut self) -> StreamEvent {
        self.started = true;
        StreamEvent::start(
            self.response_id.clone(),
            self.request_id.clone(),
            self.model.clone(),
        )
    }

    /// Translate one JSON frame. May return several events or a classified
    /// error embedded in the stream.
    pub fn translate(&mut self, payload: &Value) -> Result<Vec<StreamEvent>, GatewayError> {
        if let Some(error) = stream_error(payload) {
            return Err(error);
        }
        if let Some(usage) = payload.get("usage").and_then(parse_usage) {
            self.usage = Some(usage);
        }

        let mut events = Vec::new();
        if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    self.delta_events(delta, &mut events)?;
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    if !reason.trim().is_empty() {
                        self.finish_reason = Some(FinishReason::from_upstream(reason));
                    }
                }
            }
        }

        // Legacy cumulative text frames: emit only the suffix.
        if let Some(text) = payload.get("text").and_then(Value::as_str) {
            if text == "[DONE]" {
                return Ok(events);
            }
            let delta = if text.starts_with(self.snapshot.as_str()) {
                text[self.snapshot.len()..].to_string()
            } else {
                text.to_string()
            };
            self.snapshot = text.to_string();
            if !delta.is_empty() {
                events.push(StreamEvent::text_delta(delta));
            }
        }
        if let Some(delta) = payload.get("delta") {
            self.delta_events(delta, &mut events)?;
        }
        Ok(events)
    }

    fn delta_events(
        &mut self,
        delta: &Value,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), GatewayError> {
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                events.push(StreamEvent::text_delta(content));
            }
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning.is_empty() {
                events.push(StreamEvent::reasoning_delta(reasoning));
            }
        }
        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            return Ok(());
        };
        for (position, raw) in tool_calls.iter().enumerate() {
            let index = raw
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(position as u64);
            let state = self.tools.entry(index).or_default();
            if let Some(id) = raw.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    state.id = id.to_string();
                }
            }
            if let Some(function) = raw.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    state.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    if state.started {
                        if !arguments.is_empty() {
                            events.push(StreamEvent::tool_call_args_delta(
                                state.id.clone(),
                                arguments,
                            ));
                        }
                    } else {
                        state.args.push_str(arguments);
                    }
                }
            }
            if !state.started && !state.name.is_empty() {
                state.started = true;
                if state.id.is_empty() {
                    state.id = format!("call_{index}");
                }
                events.push(tool_start(state, index));
                if !state.args.is_empty() {
                    // Arguments that arrived before the name are emitted once the
                    // start event exists; clients key fragments by call id.
                    let args = std::mem::take(&mut state.args);
                    events.push(StreamEvent::tool_call_args_delta(state.id.clone(), args));
                }
            }
        }
        Ok(())
    }

    /// Flush buffered tool calls; called once at the end of the upstream stream.
    fn flush_tools(&mut self, events: &mut Vec<StreamEvent>) {
        for (index, state) in self.tools.iter_mut() {
            if state.started {
                continue;
            }
            if state.id.is_empty() {
                state.id = format!("call_{index}");
            }
            state.started = true;
            events.push(tool_start(state, *index));
            if !state.args.is_empty() {
                let args = std::mem::take(&mut state.args);
                events.push(StreamEvent::tool_call_args_delta(state.id.clone(), args));
            }
        }
    }

    /// Terminal events: buffered tool starts, usage, finish. Idempotent.
    pub fn finish_events(&mut self) -> Vec<StreamEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        self.flush_tools(&mut events);
        if let Some(usage) = self.usage {
            events.push(StreamEvent::usage(usage));
        }
        let reason = self.finish_reason.take().unwrap_or(FinishReason::Stop);
        events.push(StreamEvent::finish(reason));
        events
    }
}

fn tool_start(state: &ToolState, index: u64) -> StreamEvent {
    StreamEvent::ToolCallStart {
        id: state.id.clone(),
        name: state.name.clone(),
        index: Some(index as u32),
    }
}

fn stream_error(payload: &Value) -> Option<GatewayError> {
    if let Some(error) = payload.get("error") {
        let message = error
            .get("message")
            .or_else(|| error.get("error_msg"))
            .or_else(|| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("CodeArts upstream error");
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Some(classify_stream_error(code, message));
    }
    let code = payload.get("error_code")?;
    let code = match code {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    if code.is_empty() || code == "0" {
        return None;
    }
    let message = payload
        .get("error_msg")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(classify_stream_error(&code, message))
}

fn parse_usage(value: &Value) -> Option<Usage> {
    let object = value.as_object()?;
    let int = |key: &str| object.get(key).and_then(Value::as_u64);
    let mut usage = Usage::new(
        int("prompt_tokens").unwrap_or(0),
        int("completion_tokens").unwrap_or(0),
    );
    usage.total_tokens = int("total_tokens");
    usage.cached_input_tokens = object
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| int("cached_tokens"));
    usage.reasoning_tokens = object
        .get("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| int("reasoning_tokens"));
    Some(usage)
}

/// Aggregate translated events into a non-streamed response.
pub fn aggregate(
    response_id: &str,
    request_id: Option<String>,
    model: &str,
    events: &[StreamEvent],
) -> ChatResponse {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: std::collections::BTreeMap<String, ToolCall> = Default::default();
    let mut usage = None;
    let mut finish = FinishReason::Stop;
    for event in events {
        match event {
            StreamEvent::TextDelta { text: part } => text.push_str(part),
            StreamEvent::ReasoningDelta { text: part } => reasoning.push_str(part),
            StreamEvent::ToolCallStart { id, name, .. } => {
                let call = tool_calls
                    .entry(id.clone())
                    .or_insert_with(|| ToolCall::new(id.clone(), name.clone(), ""));
                if call.name.is_empty() {
                    call.name = name.clone();
                }
            }
            StreamEvent::ToolCallArgsDelta { id, delta } => {
                tool_calls
                    .entry(id.clone())
                    .or_insert_with(|| ToolCall::new(id.clone(), "", ""))
                    .arguments
                    .push_str(delta);
            }
            StreamEvent::Usage { usage: value } => usage = Some(*value),
            StreamEvent::Finish { reason } => finish = reason.clone(),
            StreamEvent::Error { .. } | StreamEvent::Start { .. } => {}
        }
    }

    let mut message = ChatMessage::new(MessageRole::Assistant, Vec::new());
    if !reasoning.is_empty() {
        message.content.push(ContentBlock::reasoning(reasoning));
    }
    if !text.is_empty() {
        message.content.push(ContentBlock::text(text));
    }
    for call in tool_calls.into_values() {
        message.content.push(ContentBlock::ToolCall(call));
    }

    let mut response =
        ChatResponse::new(response_id.to_string(), model.to_string(), message, finish);
    response.request_id = request_id;
    response.usage = usage;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChatMessage, ToolDefinition};

    fn request() -> ChatRequest {
        let mut request = ChatRequest::new(
            "glm-5.2",
            vec![ChatMessage::system("be brief"), ChatMessage::user("hello")],
        );
        request.max_output_tokens = Some(1024);
        request.temperature = Some(0.2);
        request.tools = vec![ToolDefinition::new(
            "read_file",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];
        request.tool_choice = Some(ToolChoice::Auto);
        request
    }

    #[test]
    fn body_uses_stable_cache_fields_and_options() {
        let body = build_body(&request(), "abc123").expect("body");
        assert_eq!(body["model"], "glm-5.2");
        assert_eq!(body["stream"], true);
        assert_eq!(body["chat_id"], "abc123");
        assert_eq!(body["prompt_cache_key"], "abc123");
        assert_eq!(body["tool_stream"], true);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn thinking_hints_are_forwarded_only_when_the_caller_set_them() {
        let mut request = request();
        assert!(build_body(&request, "abc123")
            .expect("body")
            .get("enable_thinking")
            .is_none());

        request.enable_thinking = Some(true);
        request.reasoning_effort = Some("medium".into());
        let body = build_body(&request, "abc123").expect("body");
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["reasoning_effort"], "medium");
    }

    #[test]
    fn tool_history_roundtrips() {
        let mut assistant = ChatMessage::assistant("checking");
        assistant.content.push(ContentBlock::ToolCall(ToolCall::new(
            "call_1",
            "read_file",
            "{\"path\":\"a\"}",
        )));
        let messages = messages_to_upstream(&[
            assistant,
            ChatMessage::tool(crate::protocol::ToolResult::new("call_1", "contents")),
        ])
        .expect("messages");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["tool_calls"][0]["type"], "function");
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(messages[0]["content"], "checking");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_1");
    }

    #[test]
    fn image_parts_become_an_openai_content_array() {
        let mut message = ChatMessage::user("what color is this");
        let part = json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}});
        message.content.push(ContentBlock::Unsupported {
            media_type: "image_url".into(),
            data: part.clone(),
        });
        let upstream = message_to_upstream(&message).expect("image message");
        assert_eq!(upstream["content"][0]["type"], "text");
        assert_eq!(upstream["content"][0]["text"], "what color is this");
        assert_eq!(upstream["content"][1], part);

        let text_only = message_to_upstream(&ChatMessage::user("plain")).expect("text");
        assert_eq!(text_only["content"], "plain");
    }

    #[test]
    fn unsupported_blocks_are_reported_not_dropped() {
        let mut message = ChatMessage::user("look");
        message.content.push(ContentBlock::Unsupported {
            media_type: "image/png".into(),
            data: json!({}),
        });
        let error = message_to_upstream(&message).expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn sse_lines_are_parsed_without_blank_line_delimiters() {
        assert!(matches!(parse_sse_line("data: [DONE]"), Ok(SseLine::Done)));
        assert!(matches!(parse_sse_line(": heartbeat"), Ok(SseLine::Ignore)));
        assert!(matches!(parse_sse_line("event: ping"), Ok(SseLine::Ignore)));
        assert!(matches!(parse_sse_line("{}"), Ok(SseLine::Ignore)));
        assert!(matches!(
            parse_sse_line(r#"data: {"choices":[]}"#),
            Ok(SseLine::Payload(_))
        ));
    }

    #[test]
    fn stream_translation_emits_content_reasoning_and_tool_fragments() {
        let mut translator = StreamTranslator::new("resp", None, "glm-5.2");
        assert!(matches!(
            translator.start_event(),
            StreamEvent::Start { .. }
        ));
        let events = translator
            .translate(&json!({
                "choices": [{"delta": {"content": "hi", "reasoning_content": "think"}}]
            }))
            .expect("events");
        assert_eq!(
            events,
            vec![
                StreamEvent::text_delta("hi"),
                StreamEvent::reasoning_delta("think")
            ]
        );

        let events = translator
            .translate(&json!({
                "choices": [{"delta": {"tool_calls": [
                    {"index": 0, "id": "call_1", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"pa"}}
                ]}}]
            }))
            .expect("tool start");
        assert!(
            matches!(&events[0], StreamEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "read_file")
        );
        assert_eq!(
            events[1],
            StreamEvent::tool_call_args_delta("call_1", "{\"pa")
        );

        let events = translator
            .translate(&json!({
                "choices": [{"delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "th\":\"a\"}"}}
                ]}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
            }))
            .expect("args");
        assert_eq!(
            events,
            vec![StreamEvent::tool_call_args_delta("call_1", "th\":\"a\"}")]
        );

        let terminal = translator.finish_events();
        assert_eq!(terminal.len(), 2);
        assert!(matches!(terminal[0], StreamEvent::Usage { usage } if usage.total() == 14));
        assert!(
            matches!(&terminal[1], StreamEvent::Finish { reason } if reason == &FinishReason::ToolCalls)
        );
    }

    #[test]
    fn legacy_snapshot_frames_only_emit_the_suffix() {
        let mut translator = StreamTranslator::new("resp", None, "m");
        let first = translator
            .translate(&json!({"text": "hello"}))
            .expect("first");
        assert_eq!(first, vec![StreamEvent::text_delta("hello")]);
        let second = translator
            .translate(&json!({"text": "hello world"}))
            .expect("second");
        assert_eq!(second, vec![StreamEvent::text_delta(" world")]);
        let repeated = translator
            .translate(&json!({"text": "hello world"}))
            .expect("noop");
        assert!(repeated.is_empty());
    }

    #[test]
    fn embedded_error_maps_to_a_classified_error() {
        let mut translator = StreamTranslator::new("resp", None, "m");
        let error = translator
            .translate(
                &json!({"error_code": "TM.00001041", "error_msg": "并发会话数已达上限(3个)"}),
            )
            .expect_err("error");
        assert_eq!(error.kind, ErrorKind::RateLimited);
    }

    #[test]
    fn aggregation_rebuilds_text_and_arguments() {
        let events = vec![
            StreamEvent::text_delta("hel"),
            StreamEvent::text_delta("lo"),
            StreamEvent::tool_call_start("call_1", "read_file"),
            StreamEvent::tool_call_args_delta("call_1", "{}"),
            StreamEvent::usage(Usage::new(3, 2)),
            StreamEvent::finish(FinishReason::ToolCalls),
        ];
        let response = aggregate("id", Some("req".into()), "m", &events);
        assert_eq!(response.text(), "hello");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(
            response.message.tool_calls().next().unwrap().arguments,
            "{}"
        );
        assert_eq!(response.usage.unwrap().total(), 5);
    }
}
