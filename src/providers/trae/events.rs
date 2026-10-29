//! TRAE SSE decoding and stream translation.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §6-§7, built from
//! `Coding2API/src/provider/trae/events.py:11-289` and
//! `dsh-wtq-pool/src/trae-sse.ts:1-231`.
//!
//! TRAE names its own SSE events instead of using OpenAI's
//! `chat.completion.chunk`:
//!
//! ```text
//! event: metadata      (model echo; no downstream meaning)
//! event: timing_cost   (no downstream meaning)
//! event: output        data: {"response":"…","reasoning_content":"…","tool_calls":[…]} ×N
//! event: extra_info    (model echo; no downstream meaning)
//! event: token_usage   data: {"prompt_tokens":…,"completion_tokens":…}
//! event: done          data: {"finish_reason":"stop"}
//! event: error         data: {"code":1005,"message":"…"}
//! ```
//!
//! Two reference behaviours are kept on purpose:
//! * tool-call fragments use SOLO's `function_call` shape and are split by
//!   index — a continuation fragment has arguments but no name and must not be
//!   dropped (`events.py:88-125`);
//! * `output` may carry `response`, `reasoning_content` and `tool_calls` in one
//!   frame; all three are forwarded (`parse_all_events`).
//!
//! `dsh` also accepts an event-less `data: {...}` frame and a bare
//! `data: [DONE]`; both are handled as a compatibility fallback.
//!
//! This module is pure: no HTTP, no tokio, no provider state.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{FinishReason, StreamError, StreamEvent, Usage};

use super::error::classify_stream_error;

pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SSE_FRAME_CHARS: usize = 8 * 1024 * 1024;

/// One decoded SSE frame. `event` is empty when the upstream sent none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser for named `event:`/`data:` frames. Bytes may split
/// anywhere, including inside a multi-byte UTF-8 character.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    event: String,
    data: String,
    data_len: usize,
    saw_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

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
                "TRAE SSE line exceeded the gateway limit",
            ));
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<Vec<SseFrame>, GatewayError> {
        let mut frames = Vec::new();
        if !self.line.is_empty() {
            let raw = std::mem::take(&mut self.line);
            if let Some(frame) = self.feed_line(&raw)? {
                frames.push(frame);
            }
        }
        if let Some(frame) = self.dispatch() {
            frames.push(frame);
        }
        Ok(frames)
    }

    fn feed_line(&mut self, raw: &[u8]) -> Result<Option<SseFrame>, GatewayError> {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|_| {
            GatewayError::upstream_server("TRAE returned invalid UTF-8 in its stream")
        })?;
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.trim().to_string(),
            "data" => {
                self.data_len += value.len() + 1;
                if self.data_len > MAX_SSE_FRAME_CHARS {
                    return Err(GatewayError::upstream_server(
                        "TRAE SSE frame exceeded the gateway limit",
                    ));
                }
                if self.saw_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.saw_data = true;
            }
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Option<SseFrame> {
        if !self.saw_data {
            self.event.clear();
            return None;
        }
        let frame = SseFrame {
            event: std::mem::take(&mut self.event),
            data: std::mem::take(&mut self.data),
        };
        self.saw_data = false;
        self.data_len = 0;
        Some(frame)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDelta {
    pub index: u64,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Default)]
struct ToolState {
    id: String,
    name: String,
    started: bool,
    args: String,
}

/// Translate TRAE frames into PolyGate [`StreamEvent`]s.
///
/// The caller emits [`StreamTranslator::start_event`] once, feeds frames, then
/// calls [`StreamTranslator::end_of_stream`] at EOF. `done` and `[DONE]` finish
/// the stream immediately: the upstream closes right after them, and waiting
/// would turn a stale connection into an idle timeout.
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

    pub fn translate(&mut self, event: &str, data: &str) -> Result<Vec<StreamEvent>, GatewayError> {
        let result = self.translate_inner(event, data);
        if result.is_err() {
            self.finished = true;
        }
        result
    }

    fn translate_inner(
        &mut self,
        event: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, GatewayError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let name = event.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(self.finish_events());
        }
        // Skip unknown events before parsing: `progress_notice` data is not
        // guaranteed to be JSON, and the reference short-circuits it too.
        if !matches!(name, "output" | "token_usage" | "done" | "error" | "") {
            return Ok(Vec::new());
        }
        let Some(payload) = parse_object(name, data)? else {
            return Ok(Vec::new());
        };
        match name {
            "output" => Ok(self.output_events(&payload)),
            "token_usage" => {
                if let Some(usage) = parse_usage(&payload) {
                    self.usage = Some(usage);
                }
                Ok(Vec::new())
            }
            "done" => {
                if let Some(reason) = payload.get("finish_reason").and_then(Value::as_str) {
                    self.finish_reason = Some(FinishReason::from_upstream(reason));
                }
                self.saw_done = true;
                Ok(self.finish_events())
            }
            "error" => {
                let code = payload.get("code").and_then(Value::as_i64);
                let message = payload.get("message").and_then(Value::as_str);
                Err(classify_stream_error(code, message))
            }
            // metadata / timing_cost / extra_info / progress_notice and future
            // names carry no downstream meaning.
            "" => {
                // dsh compatibility: an event-less frame whose payload looks
                // like an `output` chunk.
                let looks_like_output = payload.get("response").is_some()
                    || payload.get("reasoning_content").is_some()
                    || payload.get("tool_calls").is_some();
                if looks_like_output {
                    Ok(self.output_events(&payload))
                } else {
                    Ok(Vec::new())
                }
            }
            _ => Ok(Vec::new()),
        }
    }

    fn output_events(&mut self, payload: &Value) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(text) = payload
            .get("response")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            events.push(StreamEvent::text_delta(text));
        }
        if let Some(text) = payload
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            events.push(StreamEvent::reasoning_delta(text));
        }
        if let Some(calls) = payload.get("tool_calls") {
            for call in parse_tool_calls(calls) {
                self.push_tool_call(call, &mut events);
            }
        }
        events
    }

    fn push_tool_call(&mut self, call: ToolDelta, events: &mut Vec<StreamEvent>) {
        let state = self.tools.entry(call.index).or_default();
        if let Some(id) = call.id.filter(|id| !id.is_empty()) {
            // The emitted ToolCallStart already carried the id downstream, so a
            // late id cannot replace it without splitting the arguments.
            if !state.started || state.id.is_empty() {
                state.id = id;
            }
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
                state.id = format!("call_{}", call.index);
            }
            events.push(StreamEvent::ToolCallStart {
                id: state.id.clone(),
                name: state.name.clone(),
                index: Some(call.index as u32),
            });
            if !state.args.is_empty() {
                let args = std::mem::take(&mut state.args);
                events.push(StreamEvent::tool_call_args_delta(state.id.clone(), args));
            }
        }
    }

    /// End after `[DONE]` or `done`. EOF with neither is truncation.
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
            "TRAE stream ended before `done` or [DONE]",
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

/// Parse one frame's JSON payload. `output`/`token_usage`/`done`/`error` must
/// carry a JSON object; an event-less frame is parsed leniently.
fn parse_object(event: &str, data: &str) -> Result<Option<Value>, GatewayError> {
    let value: Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(_) if event.is_empty() => return Ok(None),
        Err(_) => {
            return Err(GatewayError::upstream_server(format!(
                "TRAE returned an unparsable SSE frame for event `{event}`"
            )))
        }
    };
    if !value.is_object() {
        if event.is_empty() {
            return Ok(None);
        }
        return Err(GatewayError::upstream_server(format!(
            "TRAE SSE frame for event `{event}` is not a JSON object"
        )));
    }
    Ok(Some(value))
}

fn parse_tool_calls(value: &Value) -> Vec<ToolDelta> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let Some(call) = item.as_object() else {
            continue;
        };
        let function = call
            .get("function_call")
            .and_then(Value::as_object)
            .or_else(|| call.get("function").and_then(Value::as_object));
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(argument_text);
        // A fragment with neither a name nor meaningful arguments is noise.
        if name.is_none() && arguments.as_deref().is_none_or(|args| args == "{}") {
            continue;
        }
        calls.push(ToolDelta {
            index: call
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(position as u64),
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

fn parse_usage(payload: &Value) -> Option<Usage> {
    let object = payload.as_object()?;
    let int = |key: &str| object.get(key).and_then(Value::as_u64);
    if !["prompt_tokens", "completion_tokens", "total_tokens"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return None;
    }
    let mut usage = Usage::new(
        int("prompt_tokens").unwrap_or(0),
        int("completion_tokens").unwrap_or(0),
    );
    usage.total_tokens = int("total_tokens");
    usage.reasoning_tokens = int("reasoning_tokens");
    // Cache-hit chain observed by the reference: OpenAI details first, then a
    // top-level `cached_tokens`, then TRAE's own `cache_read_input_tokens`
    // (a real 0 is a meaningful value and stays 0).
    usage.cached_input_tokens = object
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| int("cached_tokens"))
        .or_else(|| int("cache_read_input_tokens"));
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_keeps_event_names_and_split_utf8() {
        let payload = "event: output\r\ndata: {\"response\":\"你好\"}\r\n\r\n";
        let (first, second) = payload.as_bytes().split_at(32);
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(first).unwrap().is_empty());
        let frames = decoder.push(second).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "output");
        assert!(frames[0].data.contains("你好"));
    }

    #[test]
    fn comment_and_metadata_frames_yield_no_events() {
        let mut translator = StreamTranslator::new("r", None, "m");
        assert!(translator
            .translate("metadata", r#"{"model":"glm-5.2"}"#)
            .unwrap()
            .is_empty());
        assert!(translator
            .translate("timing_cost", r#"{"provider_model_name":"x"}"#)
            .unwrap()
            .is_empty());
        assert!(translator
            .translate("progress_notice", "not json")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn output_carries_content_reasoning_and_tools() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator
            .translate(
                "output",
                r#"{"response":"hi","reasoning_content":"why","tool_calls":[{"index":0,"id":"c1","function_call":{"name":"bash","arguments":"{\"a\":1}"}}]}"#,
            )
            .unwrap();
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], StreamEvent::ReasoningDelta { .. }));
        assert!(matches!(events[2], StreamEvent::ToolCallStart { .. }));
    }

    #[test]
    fn a_tool_continuation_without_a_name_is_kept() {
        let mut translator = StreamTranslator::new("r", None, "m");
        translator
            .translate(
                "output",
                r#"{"tool_calls":[{"index":0,"id":"c1","function_call":{"name":"bash","arguments":"{\"a\":"}}]}"#,
            )
            .unwrap();
        let events = translator
            .translate(
                "output",
                r#"{"tool_calls":[{"index":0,"function_call":{"arguments":"1}"}}]}"#,
            )
            .unwrap();
        assert!(matches!(
            events[0],
            StreamEvent::ToolCallArgsDelta { ref delta, .. } if delta == "1}"
        ));
    }

    #[test]
    fn blank_tool_fragments_are_noise() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator
            .translate(
                "output",
                r#"{"tool_calls":[{"index":0,"function_call":{"arguments":"{}"}}]}"#,
            )
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn usage_keeps_cache_read_and_zero() {
        let mut translator = StreamTranslator::new("r", None, "m");
        translator
            .translate(
                "token_usage",
                r#"{"prompt_tokens":10,"completion_tokens":2,"reasoning_tokens":1,"cache_read_input_tokens":0}"#,
            )
            .unwrap();
        let tail = translator
            .translate("done", r#"{"finish_reason":"stop"}"#)
            .unwrap();
        let usage = tail
            .iter()
            .find_map(|event| match event {
                StreamEvent::Usage { usage } => Some(*usage),
                _ => None,
            })
            .expect("usage event");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.reasoning_tokens, Some(1));
        assert_eq!(usage.cached_input_tokens, Some(0));
    }

    #[test]
    fn done_finishes_immediately_and_eof_is_then_clean() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator
            .translate("done", r#"{"finish_reason":"length"}"#)
            .unwrap();
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Finish {
                reason: FinishReason::Length
            })
        ));
        assert!(translator.end_of_stream().is_empty());
    }

    #[test]
    fn done_without_reason_is_stop() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator.translate("done", "{}").unwrap();
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Finish {
                reason: FinishReason::Stop
            })
        ));
    }

    #[test]
    fn stream_error_frames_are_classified() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let error = translator
            .translate("error", r#"{"code":1005,"message":"plan"}"#)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ChannelRestricted);
        assert!(translator
            .translate("output", r#"{"response":"x"}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn truncation_without_done_is_an_error() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator.end_of_stream();
        assert!(matches!(events[0], StreamEvent::Error { .. }));
    }

    #[test]
    fn eventless_frames_are_accepted_as_output() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator.translate("", r#"{"response":"x"}"#).unwrap();
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
    }

    #[test]
    fn done_data_frame_terminates_the_stream() {
        let mut translator = StreamTranslator::new("r", None, "m");
        let events = translator.translate("", "[DONE]").unwrap();
        assert!(matches!(events.last(), Some(StreamEvent::Finish { .. })));
    }
}
