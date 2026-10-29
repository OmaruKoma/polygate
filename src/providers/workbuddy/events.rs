//! WorkBuddy SSE decoding and stream translation.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §8. The upstream uses plain OpenAI
//! SSE frames (`data: {...}` / `data: [DONE]`), not the Qoder envelope. Empty
//! placeholders (`tool_calls: []`, `function_call: null`, `refusal: ""`) are
//! ignored so a client never sees a spurious tool call.
//!
//! `finish_reason: "content-filter"` (hyphen) is mapped to
//! [`FinishReason::ContentFilter`] — it is a normal end of turn, not a
//! transport error. A stream that ends without `[DONE]` or a `finish_reason` is
//! reported as truncation.
//!
//! This module is pure: no HTTP, no tokio, no provider state.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{FinishReason, StreamError, StreamEvent, Usage};

use super::error::classify_payload;

pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SSE_FRAME_CHARS: usize = 8 * 1024 * 1024;

/// Incremental SSE parser for `data:` frames. Bytes may split anywhere,
/// including inside a multi-byte UTF-8 character.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: String,
    data_len: usize,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, GatewayError> {
        self.line.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(position) = self.line.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.line.drain(..=position).collect();
            if let Some(frame) = self.feed_line(&raw)? {
                frames.push(frame);
            }
        }
        if self.line.len() > MAX_SSE_LINE_BYTES {
            return Err(GatewayError::upstream_server(
                "WorkBuddy SSE line exceeded the gateway limit",
            ));
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<Vec<String>, GatewayError> {
        let mut frames = Vec::new();
        if !self.line.is_empty() {
            let raw = std::mem::take(&mut self.line);
            if let Some(frame) = self.feed_line(&raw)? {
                frames.push(frame);
            }
        }
        if let Some(frame) = self.flush() {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn feed_line(&mut self, raw: &[u8]) -> Result<Option<String>, GatewayError> {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy returned invalid UTF-8 in its stream")
        })?;
        if line.is_empty() {
            return Ok(self.flush());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let Some(value) = line.strip_prefix("data:") else {
            return Ok(None);
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        self.data_len += value.len();
        if self.data_len > MAX_SSE_FRAME_CHARS {
            return Err(GatewayError::upstream_server(
                "WorkBuddy SSE frame exceeded the gateway limit",
            ));
        }
        if !self.data.is_empty() {
            self.data.push('\n');
        }
        self.data.push_str(value);
        Ok(None)
    }

    fn flush(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        self.data_len = 0;
        Some(std::mem::take(&mut self.data))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolDelta>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDelta {
    pub index: Option<u64>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// Parse one `data:` payload. An embedded error envelope becomes an error even
/// when it arrives inside an otherwise healthy stream.
pub fn parse_chunk(data: &str) -> Result<Chunk, GatewayError> {
    let payload: Value = serde_json::from_str(data)
        .map_err(|_| GatewayError::upstream_server("WorkBuddy returned an unparsable SSE chunk"))?;
    if let Some(error) = classify_payload(&payload) {
        return Err(error);
    }
    let object = payload
        .as_object()
        .ok_or_else(|| GatewayError::upstream_server("WorkBuddy SSE chunk is not a JSON object"))?;

    let mut chunk = Chunk {
        usage: object.get("usage").and_then(parse_usage),
        ..Chunk::default()
    };
    let choice = object
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object);
    let Some(choice) = choice else {
        return Ok(chunk);
    };
    if let Some(delta) = choice.get("delta").and_then(Value::as_object) {
        chunk.content = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        chunk.reasoning = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        chunk.tool_calls = parse_tool_calls(delta);
    }
    chunk.finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(chunk)
}

fn parse_tool_calls(delta: &serde_json::Map<String, Value>) -> Vec<ToolDelta> {
    let Some(items) = delta.get("tool_calls").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let Some(call) = item.as_object() else {
            continue;
        };
        let function = call.get("function").and_then(Value::as_object);
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(argument_text);
        if name.is_none() && arguments.is_none() {
            continue;
        }
        calls.push(ToolDelta {
            index: call
                .get("index")
                .and_then(Value::as_u64)
                .or(Some(position as u64)),
            id: call
                .get("id")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            name,
            arguments,
        });
    }
    calls
}

fn argument_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(map) if !map.is_empty() => serde_json::to_string(value).ok(),
        Value::Array(items) if !items.is_empty() => serde_json::to_string(value).ok(),
        _ => None,
    }
}

fn parse_usage(value: &Value) -> Option<Usage> {
    let object = value.as_object()?;
    let has_counts = ["prompt_tokens", "completion_tokens", "total_tokens"]
        .iter()
        .any(|key| object.contains_key(*key));
    if !has_counts {
        return None;
    }
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

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    started: bool,
    args: String,
}

/// Translate WorkBuddy chunks into PolyGate [`StreamEvent`]s.
///
/// The caller emits [`StreamTranslator::start_event`] once, feeds frames, then
/// calls [`StreamTranslator::end_of_stream`] at EOF.
pub struct StreamTranslator {
    response_id: String,
    request_id: Option<String>,
    model: String,
    tools: BTreeMap<u64, ToolState>,
    usage: Option<Usage>,
    finish_reason: Option<FinishReason>,
    saw_done: bool,
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
            tools: BTreeMap::new(),
            usage: None,
            finish_reason: None,
            saw_done: false,
            finished: false,
        }
    }

    pub fn start_event(&self) -> StreamEvent {
        StreamEvent::start(
            self.response_id.clone(),
            self.request_id.clone(),
            self.model.clone(),
        )
    }

    pub fn translate(&mut self, data: &str) -> Result<Vec<StreamEvent>, GatewayError> {
        let result = self.translate_inner(data);
        if result.is_err() {
            self.finished = true;
        }
        result
    }

    fn translate_inner(&mut self, data: &str) -> Result<Vec<StreamEvent>, GatewayError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(self.finish_events());
        }
        let chunk = parse_chunk(data)?;
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        if let Some(reason) = chunk.finish_reason {
            self.finish_reason = Some(FinishReason::from_upstream(&reason));
        }
        let mut events = Vec::new();
        if let Some(text) = chunk.content {
            events.push(StreamEvent::text_delta(text));
        }
        if let Some(text) = chunk.reasoning {
            events.push(StreamEvent::reasoning_delta(text));
        }
        for call in chunk.tool_calls {
            self.push_tool_call(call, &mut events);
        }
        Ok(events)
    }

    fn push_tool_call(&mut self, call: ToolDelta, events: &mut Vec<StreamEvent>) {
        let index = call.index.unwrap_or(0);
        let state = self.tools.entry(index).or_default();
        if let Some(id) = call.id.filter(|id| !id.is_empty()) {
            state.id = id;
        }
        if let Some(name) = call.name {
            state.name.push_str(&name);
        }
        if let Some(arguments) = call.arguments.filter(|args| !args.is_empty()) {
            if state.started {
                events.push(StreamEvent::tool_call_args_delta(
                    state.id.clone(),
                    arguments,
                ));
            } else {
                state.args.push_str(&arguments);
            }
        }
        if !state.started && !state.name.is_empty() {
            state.started = true;
            if state.id.is_empty() {
                state.id = format!("call_{index}");
            }
            events.push(StreamEvent::ToolCallStart {
                id: state.id.clone(),
                name: state.name.clone(),
                index: Some(index as u32),
            });
            if !state.args.is_empty() {
                let args = std::mem::take(&mut state.args);
                events.push(StreamEvent::tool_call_args_delta(state.id.clone(), args));
            }
        }
    }

    /// End after `[DONE]` or a `finish_reason`. EOF with neither is truncation.
    pub fn end_of_stream(&mut self) -> Vec<StreamEvent> {
        if self.finished {
            return Vec::new();
        }
        if self.saw_done || self.finish_reason.is_some() {
            return self.finish_events();
        }
        self.finished = true;
        vec![StreamEvent::error(StreamError::new(
            ErrorKind::UpstreamServer,
            "WorkBuddy stream ended before [DONE] or finish_reason",
        ))]
    }

    fn finish_events(&mut self) -> Vec<StreamEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        for (index, state) in self.tools.iter_mut() {
            if state.started {
                continue;
            }
            if state.id.is_empty() {
                state.id = format!("call_{index}");
            }
            state.started = true;
            events.push(StreamEvent::ToolCallStart {
                id: state.id.clone(),
                name: state.name.clone(),
                index: Some(*index as u32),
            });
            if !state.args.is_empty() {
                let args = std::mem::take(&mut state.args);
                events.push(StreamEvent::tool_call_args_delta(state.id.clone(), args));
            }
        }
        if let Some(usage) = self.usage.take() {
            events.push(StreamEvent::usage(usage));
        }
        let reason = self.finish_reason.take().unwrap_or(FinishReason::Stop);
        events.push(StreamEvent::finish(reason));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_accepts_split_utf8_and_crlf() {
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\r\n\r\n";
        let (first, second) = payload.as_bytes().split_at(30);
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(first).unwrap().is_empty());
        let frames = decoder.push(second).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("你好"));
    }

    #[test]
    fn empty_tool_placeholders_do_not_start_a_call() {
        let chunk = parse_chunk(
            r#"{"choices":[{"delta":{"content":"hi","tool_calls":[],"function_call":null}}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.content.as_deref(), Some("hi"));
        assert!(chunk.tool_calls.is_empty());
    }

    #[test]
    fn content_filter_is_a_normal_finish() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator
            .translate(
                r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"content-filter"}]}"#,
            )
            .unwrap();
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
        let tail = translator.end_of_stream();
        assert!(matches!(
            tail.last(),
            Some(StreamEvent::Finish {
                reason: FinishReason::ContentFilter
            })
        ));
    }

    #[test]
    fn truncation_without_done_or_finish_is_an_error() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator.end_of_stream();
        assert!(matches!(events[0], StreamEvent::Error { .. }));
    }
}
