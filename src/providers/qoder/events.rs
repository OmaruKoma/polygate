//! Qoder SSE envelope decoding and stream translation.
//!
//! Source: Coding2API `src/engine/sse.py` (frame state machine),
//! `src/provider/qoder/events.py` (`decode_envelope`, `is_keepalive_body`,
//! `parse_inner_chunk`, `clean_delta`) and `src/provider/qoder/client.py`
//! (`_decode_frame`) at commit `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! Qoder wraps each OpenAI chunk in an outer JSON envelope:
//!
//! ```text
//! data: {"statusCodeValue":200,"body":"{\"choices\":[...]}","headers":...}
//! ```
//!
//! The inner `body` is a JSON **string**, not an object. A keepalive frame has
//! `body:"null"` and must be skipped, not treated as a protocol violation.
//! Stream end is `[DONE]` (top level or inner body) or a `finish_reason`.
//!
//! This module is pure: no HTTP, no tokio, no provider state.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{FinishReason, StreamError, StreamEvent, Usage};

use super::error::classify_payload;

pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SSE_FRAME_CHARS: usize = 8 * 1024 * 1024;

/// Quota sentinels the official clients recognise at the top of a `data:` frame.
/// `[EXCEED_QUOTA]` means the request ran out of credits; `[NOT_EXCEED_QUOTA]`
/// is emitted while it has not.
pub const EXCEEDED_MARKER: &str = "[EXCEED_QUOTA]";
pub const NOT_EXCEEDED_MARKER: &str = "[NOT_EXCEED_QUOTA]";

/// One assembled SSE frame (`event:` + concatenated `data:` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser. Bytes may be split anywhere, including inside a
/// multi-byte UTF-8 character: lines are only decoded after a complete `\n`.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    event: String,
    data_parts: Vec<String>,
    data_len: usize,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one network chunk; returns every frame completed by this chunk.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, GatewayError> {
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
                "Qoder SSE line exceeded the gateway limit",
            ));
        }
        Ok(frames)
    }

    /// Flush a final line without a trailing newline and any pending frame.
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, GatewayError> {
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

    fn feed_line(&mut self, raw: &[u8]) -> Result<Option<SseFrame>, GatewayError> {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|_| {
            GatewayError::upstream_server("Qoder returned invalid UTF-8 in its stream")
        })?;
        if line.is_empty() {
            return Ok(self.flush());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                self.data_len += value.len();
                if self.data_len > MAX_SSE_FRAME_CHARS {
                    return Err(GatewayError::upstream_server(
                        "Qoder SSE frame exceeded the gateway limit",
                    ));
                }
                self.data_parts.push(value.to_string());
            }
            _ => {}
        }
        Ok(None)
    }

    fn flush(&mut self) -> Option<SseFrame> {
        if self.data_parts.is_empty() {
            self.event.clear();
            return None;
        }
        let frame = SseFrame {
            event: std::mem::take(&mut self.event),
            data: self.data_parts.join("\n"),
        };
        self.data_parts.clear();
        self.data_len = 0;
        Some(frame)
    }
}

/// Decoded outer envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub status: i64,
    pub body: String,
}

/// Decode one frame. Empty frames and a 200 envelope without a string body are
/// keepalives; `[DONE]` is normalised into a 200 envelope.
pub fn decode_envelope(frame: &SseFrame) -> Result<Option<Envelope>, GatewayError> {
    let data = frame.data.trim();
    if data.is_empty() {
        return Ok(None);
    }
    // The official clients drop notification frames before parsing; they are
    // telemetry noise, not chat content. The "not exceeded" sentinel is a
    // keepalive for the same reason.
    if data.starts_with("[NOTIFICATIONS]#") || data == NOT_EXCEEDED_MARKER {
        return Ok(None);
    }
    // `[EXCEED_QUOTA]` is a bare top-level sentinel (not JSON). Treat it as a
    // quota condition with the same status the HTTP path uses, so a stream that
    // stops here becomes `QuotaExhausted` instead of a protocol violation.
    if data.starts_with(EXCEEDED_MARKER) {
        return Ok(Some(Envelope {
            status: 402,
            body: data.to_string(),
        }));
    }
    if data == "[DONE]" {
        return Ok(Some(Envelope {
            status: 200,
            body: "[DONE]".into(),
        }));
    }
    let outer: Value = serde_json::from_str(data)
        .map_err(|_| GatewayError::upstream_server("Qoder returned an unparsable SSE envelope"))?;
    let object = outer
        .as_object()
        .ok_or_else(|| GatewayError::upstream_server("Qoder SSE envelope is not an object"))?;
    let status = status_code(object.get("statusCodeValue"));
    match object.get("body") {
        Some(Value::String(body)) => Ok(Some(Envelope {
            status,
            body: body.clone(),
        })),
        _ if status == 200 => Ok(None),
        _ => Ok(Some(Envelope {
            status,
            body: serde_json::to_string(&outer).unwrap_or_default(),
        })),
    }
}

/// A keepalive is `null` or an empty object/array; anything else is content.
pub fn is_keepalive_body(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn status_code(value: Option<&Value>) -> i64 {
    let Some(value) = value else {
        return 200;
    };
    if value.is_boolean() {
        return 502;
    }
    if let Some(number) = value.as_i64() {
        return number;
    }
    value
        .as_str()
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(502)
}

/// One inner OpenAI chunk. Text/reasoning/usage/finish may all arrive in the
/// same frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QoderChunk {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<QoderToolDelta>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QoderToolDelta {
    pub index: Option<u64>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// Parse an inner chunk. Incomplete argument JSON is kept as a raw fragment;
/// this function never deserializes it.
pub fn parse_inner_chunk(body: &str) -> Result<QoderChunk, GatewayError> {
    let payload: Value = serde_json::from_str(body)
        .map_err(|_| GatewayError::upstream_server("Qoder returned an unparsable inner chunk"))?;
    let object = payload
        .as_object()
        .ok_or_else(|| GatewayError::upstream_server("Qoder inner chunk is not an object"))?;

    if let Some(error) = classify_payload(&payload) {
        return Err(error);
    }

    let choice = object
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object);

    let mut chunk = QoderChunk {
        usage: object.get("usage").and_then(parse_usage),
        model: object
            .get("model")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        ..QoderChunk::default()
    };

    if let Some(choice) = choice {
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
    }

    Ok(chunk)
}

fn parse_tool_calls(delta: &serde_json::Map<String, Value>) -> Vec<QoderToolDelta> {
    let mut calls = Vec::new();
    if let Some(raw_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for (position, call) in raw_calls.iter().enumerate() {
            let Some(call) = call.as_object() else {
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
            if is_blank_call(name.as_deref(), arguments.as_deref()) {
                continue;
            }
            calls.push(QoderToolDelta {
                // OpenAI deltas normally carry `index`; when they do not, the
                // array position is the only stable key and prevents two calls
                // from being merged into index 0.
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
    }

    // Older responses use a singular `function_call`; normalise it into a call
    // with no index. `clean_delta` in the reference does the same.
    if let Some(function) = delta.get("function_call").and_then(Value::as_object) {
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let arguments = function.get("arguments").and_then(argument_text);
        if !is_blank_call(name.as_deref(), arguments.as_deref()) {
            calls.push(QoderToolDelta {
                index: None,
                id: None,
                name,
                arguments,
            });
        }
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

fn is_blank_call(name: Option<&str>, arguments: Option<&str>) -> bool {
    name.is_none_or(|name| name.is_empty())
        && arguments.is_none_or(|arguments| arguments.is_empty())
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

/// Translate Qoder frames into PolyGate [`StreamEvent`]s.
///
/// The caller emits [`QoderStreamTranslator::start_event`] once before feeding
/// frames, then calls [`QoderStreamTranslator::end_of_stream`] at EOF.
pub struct QoderStreamTranslator {
    response_id: String,
    request_id: Option<String>,
    model: String,
    tools: BTreeMap<u64, ToolState>,
    usage: Option<Usage>,
    finish_reason: Option<FinishReason>,
    saw_done: bool,
    finished: bool,
}

impl QoderStreamTranslator {
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

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Translate one frame. A malformed frame or an embedded upstream error is
    /// returned as `Err`; the caller turns it into a terminal stream error.
    pub fn translate_frame(&mut self, frame: &SseFrame) -> Result<Vec<StreamEvent>, GatewayError> {
        let result = self.translate_frame_inner(frame);
        if result.is_err() {
            self.finished = true;
        }
        result
    }

    fn translate_frame_inner(
        &mut self,
        frame: &SseFrame,
    ) -> Result<Vec<StreamEvent>, GatewayError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let Some(envelope) = decode_envelope(frame)? else {
            return Ok(Vec::new());
        };
        if envelope.body == "[DONE]" {
            self.saw_done = true;
            return Ok(self.finish_events());
        }
        if envelope.status != 200 {
            return Err(super::error::classify_envelope(
                envelope.status,
                &envelope.body,
            ));
        }
        // The same sentinels can be wrapped in an envelope's `body`.
        if envelope.body.trim() == NOT_EXCEEDED_MARKER {
            return Ok(Vec::new());
        }
        if envelope.body.trim_start().starts_with(EXCEEDED_MARKER) {
            let body = envelope.body.trim().to_string();
            return Err(super::error::classify_envelope(402, &body));
        }
        if is_keepalive_body(&envelope.body) {
            return Ok(Vec::new());
        }

        let chunk = parse_inner_chunk(&envelope.body)?;
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

    fn push_tool_call(&mut self, call: QoderToolDelta, events: &mut Vec<StreamEvent>) {
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

    /// End after `[DONE]` or EOF. EOF without `[DONE]` and without a
    /// `finish_reason` is an abnormal truncation, not a successful stop.
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
            "Qoder stream ended before [DONE] or finish_reason",
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
