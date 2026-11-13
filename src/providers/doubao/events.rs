//! Doubao SSE decoding and stream translation.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md` §4. The upstream does not stream OpenAI
//! deltas; it streams **content-block patches**:
//!
//! ```text
//! event: SSE_ACK            ack_client_meta.conversation_id
//! event: STREAM_CHUNK       patch_op[].patch_value.content_block[]
//! event: STREAM_MSG_NOTIFY  content.content_block[]
//! data: {"text": "..."}     compact delta (CHUNK_DELTA)
//! event: STREAM_ERROR       {"error_code":..., "error_msg":...}
//! ```
//!
//! Block routing: the **first** `block_type=10040` container opens the reasoning
//! chain, the **second** one opens the visible answer. Text arriving in between
//! belongs to whichever container is currently open.
//!
//! This module is pure: no HTTP, no tokio, no provider state.

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{FinishReason, StreamEvent};

use super::payload;

pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;

/// One decoded SSE event (`event:` plus joined `data:` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Event>, GatewayError> {
        self.line.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(position) = self.line.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.line.drain(..=position).collect();
            if let Some(event) = self.feed_line(&raw)? {
                events.push(event);
            }
        }
        if self.line.len() > MAX_SSE_LINE_BYTES {
            return Err(GatewayError::new(
                ErrorKind::UpstreamServer,
                "Doubao SSE line exceeded the gateway limit",
            ));
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<Event>, GatewayError> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            let raw = std::mem::take(&mut self.line);
            if let Some(event) = self.feed_line(&raw)? {
                events.push(event);
            }
        }
        if let Some(event) = self.flush() {
            events.push(event);
        }
        Ok(events)
    }

    fn feed_line(&mut self, raw: &[u8]) -> Result<Option<Event>, GatewayError> {
        let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|_| {
            GatewayError::new(
                ErrorKind::UpstreamServer,
                "Doubao returned invalid UTF-8 in its stream",
            )
        })?;
        if line.is_empty() {
            return Ok(self.flush());
        }
        if line.starts_with(':') {
            // Comment / heartbeat; the reference project ignores these too.
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        Ok(None)
    }

    fn flush(&mut self) -> Option<Event> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let event = Event {
            name: self.event.take(),
            data: self.data.join("\n"),
        };
        self.data.clear();
        Some(event)
    }
}

/// Upstream business error frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError {
    pub code: i64,
    pub message: String,
}

/// Maps content-block patches onto [`StreamEvent`]s.
pub struct StreamTranslator {
    started: bool,
    finished: bool,
    thinking_containers: u32,
    in_thinking: bool,
    response_id: String,
    model: String,
    usage_stext: usize,
}

impl StreamTranslator {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            started: false,
            finished: false,
            thinking_containers: 0,
            in_thinking: false,
            response_id: String::new(),
            model: model.into(),
            usage_stext: 0,
        }
    }

    fn ensure_started(&mut self, events: &mut Vec<StreamEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let id = if self.response_id.is_empty() {
            format!("doubao-{}", uuid::Uuid::new_v4())
        } else {
            self.response_id.clone()
        };
        events.push(StreamEvent::Start {
            id,
            request_id: None,
            model: self.model.clone(),
        });
    }

    /// One SSE event in, zero or more internal events out.
    pub fn push(&mut self, event: &Event) -> Result<Vec<StreamEvent>, GatewayError> {
        let mut events = Vec::new();
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(events);
        }

        let value: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            // Non-JSON frames (heartbeats, plain text) are ignored rather than
            // aborting a stream the client may still be reading.
            Err(_) => return Ok(events),
        };

        // Business error: either an explicit STREAM_ERROR event or an inline
        // `error_code` on any frame.
        if let Some(error) = upstream_error(event, &value) {
            self.ensure_started(&mut events);
            events.push(StreamEvent::Error {
                error: crate::protocol::StreamError::new(
                    classify(error.code),
                    format!("Doubao error {}: {}", error.code, error.message),
                ),
            });
            self.finished = true;
            return Ok(events);
        }

        if event.name.as_deref() == Some("SSE_ACK") {
            if let Some(id) = value
                .get("ack_client_meta")
                .and_then(|meta| meta.get("conversation_id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && *id != "0")
            {
                self.response_id = id.to_string();
            }
            return Ok(events);
        }

        // Compact delta: `{"text": "..."}`.
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                self.ensure_started(&mut events);
                self.emit_text(text, &mut events);
            }
            return Ok(events);
        }

        let mut saw_block = false;
        for block in iter_blocks(&value) {
            let block_type = block
                .get("block_type")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            match block_type {
                payload::BLOCK_THINKING => {
                    self.thinking_containers += 1;
                    self.in_thinking = self.thinking_containers == 1;
                    saw_block = true;
                }
                10_000 => {
                    if let Some(text) = block
                        .get("content")
                        .and_then(|content| content.get("text_block"))
                        .and_then(|text_block| text_block.get("text"))
                        .and_then(Value::as_str)
                    {
                        if !text.is_empty() {
                            self.ensure_started(&mut events);
                            self.emit_text(text, &mut events);
                        }
                    }
                    saw_block = true;
                }
                // Tool / search / loading notices carry no answer text. They are
                // acknowledged (so the stream is considered started) but never
                // mapped onto visible content: this adapter has no tool surface.
                10_024 | 10_025 | 10_101 => saw_block = true,
                _ => {}
            }
        }
        if saw_block {
            self.ensure_started(&mut events);
        }
        Ok(events)
    }

    fn emit_text(&mut self, text: &str, events: &mut Vec<StreamEvent>) {
        self.usage_stext += text.chars().count();
        if self.in_thinking {
            events.push(StreamEvent::ReasoningDelta {
                text: text.to_string(),
            });
        } else {
            events.push(StreamEvent::TextDelta {
                text: text.to_string(),
            });
        }
    }

    /// Terminal event. The upstream never reports an OpenAI-style finish
    /// reason, so this is `stop` once text was produced and `unknown` otherwise.
    pub fn finish(&mut self) -> Option<StreamEvent> {
        if self.finished {
            return None;
        }
        self.finished = true;
        let reason = if self.started && self.usage_stext > 0 {
            FinishReason::Stop
        } else {
            FinishReason::Unknown
        };
        Some(StreamEvent::Finish { reason })
    }

    pub fn started(&self) -> bool {
        self.started
    }
}

/// Extract an upstream business error, if any.
pub fn upstream_error(event: &Event, value: &Value) -> Option<UpstreamError> {
    if event.name.as_deref() == Some("STREAM_ERROR") {
        let code = value
            .get("error_code")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let message = value
            .get("error_msg")
            .and_then(Value::as_str)
            .unwrap_or("upstream error")
            .to_string();
        return Some(UpstreamError { code, message });
    }
    let code = value.get("error_code").and_then(Value::as_i64)?;
    if code == 0 {
        return None;
    }
    Some(UpstreamError {
        code,
        message: value
            .get("error_msg")
            .and_then(Value::as_str)
            .unwrap_or("upstream error")
            .to_string(),
    })
}

/// Content blocks arrive in three shapes: inside `patch_op[].patch_value`,
/// inside `content.content_block`, or (legacy) directly as `content_block`.
fn iter_blocks(value: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    for patch in value
        .get("patch_op")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        if let Some(list) = patch
            .get("patch_value")
            .and_then(|patch_value| patch_value.get("content_block"))
            .and_then(Value::as_array)
        {
            blocks.extend(list.iter().cloned());
        }
        // Newer shape: `patch_value.content` is a JSON string holding `text`.
        if let Some(inner) = patch
            .get("patch_value")
            .and_then(|patch_value| patch_value.get("content"))
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        {
            if inner.get("text").and_then(Value::as_str).is_some() {
                blocks.push(inner);
            }
        }
    }
    if let Some(list) = value
        .get("content")
        .and_then(|content| content.get("content_block"))
        .and_then(Value::as_array)
    {
        blocks.extend(list.iter().cloned());
    }
    if let Some(list) = value.get("content_block").and_then(Value::as_array) {
        blocks.extend(list.iter().cloned());
    }
    blocks
}

/// Upstream business codes seen so far.
pub fn classify(code: i64) -> ErrorKind {
    match code {
        710022004 => ErrorKind::RateLimited,
        710022005 | 710022006 => ErrorKind::AuthExpired,
        _ => ErrorKind::UpstreamServer,
    }
}

/// Aggregate translated events into one response (the upstream only streams).
pub fn aggregate(
    response_id: &str,
    model: &str,
    events: &[StreamEvent],
) -> Result<crate::protocol::ChatResponse, GatewayError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut finish = None;
    for event in events {
        match event {
            StreamEvent::TextDelta { text: part } => text.push_str(part),
            StreamEvent::ReasoningDelta { text: part } => reasoning.push_str(part),
            StreamEvent::Finish { reason } => finish = Some(reason.clone()),
            StreamEvent::Error { error } => {
                return Err(GatewayError::new(error.kind, error.message.clone()));
            }
            _ => {}
        }
    }
    let mut message =
        crate::protocol::ChatMessage::new(crate::protocol::MessageRole::Assistant, Vec::new());
    if !reasoning.is_empty() {
        message
            .content
            .push(crate::protocol::ContentBlock::reasoning(reasoning));
    }
    if !text.is_empty() {
        message
            .content
            .push(crate::protocol::ContentBlock::text(text));
    }
    Ok(crate::protocol::ChatResponse::new(
        response_id.to_string(),
        model.to_string(),
        message,
        finish.unwrap_or(FinishReason::Unknown),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(name: &str, value: Value) -> Event {
        Event {
            name: Some(name.to_string()),
            data: value.to_string(),
        }
    }

    #[test]
    fn the_decoder_joins_multi_line_data_and_ignores_heartbeats() {
        let mut decoder = SseDecoder::new();
        let frames = decoder
            .push(b": heartbeat\n\nevent: STREAM_CHUNK\ndata: {\"a\":\ndata: 1}\n\n")
            .expect("decode");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].name.as_deref(), Some("STREAM_CHUNK"));
        assert_eq!(frames[0].data, "{\"a\":\n1}");
    }

    #[test]
    fn the_first_thinking_container_is_reasoning_and_the_second_is_the_answer() {
        let mut translator = StreamTranslator::new("doubao-think");
        let reasoning = translator
            .push(&event(
                "STREAM_CHUNK",
                json!({"patch_op": [{"patch_value": {"content_block": [
                    {"block_type": 10040},
                    {"block_type": 10000, "content": {"text_block": {"text": "think"}}}
                ]}}]}),
            ))
            .expect("chunk");
        assert!(matches!(
            reasoning.last(),
            Some(StreamEvent::ReasoningDelta { .. })
        ));

        let answer = translator
            .push(&event(
                "STREAM_CHUNK",
                json!({"patch_op": [{"patch_value": {"content_block": [
                    {"block_type": 10040},
                    {"block_type": 10000, "content": {"text_block": {"text": "answer"}}}
                ]}}]}),
            ))
            .expect("chunk");
        assert!(matches!(answer.last(), Some(StreamEvent::TextDelta { .. })));
    }

    #[test]
    fn a_compact_delta_is_visible_text_before_any_thinking_block() {
        let mut translator = StreamTranslator::new("doubao");
        let events = translator
            .push(&Event {
                name: None,
                data: json!({"text": "hello"}).to_string(),
            })
            .expect("delta");
        assert!(events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { text } if text == "hello")));
    }

    #[test]
    fn a_business_error_becomes_a_stream_error_not_silence() {
        let mut translator = StreamTranslator::new("doubao");
        let events = translator
            .push(&event(
                "STREAM_ERROR",
                json!({"error_code": 710022004, "error_msg": "rate limited"}),
            ))
            .expect("error frame");
        let error = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::Error { error } => Some(error.clone()),
                _ => None,
            })
            .expect("error event");
        assert_eq!(error.kind, ErrorKind::RateLimited);
        assert!(translator.finish().is_none(), "an error ends the stream");
    }

    #[test]
    fn an_empty_stream_finishes_as_unknown_rather_than_stop() {
        let mut translator = StreamTranslator::new("doubao");
        assert!(matches!(
            translator.finish(),
            Some(StreamEvent::Finish {
                reason: FinishReason::Unknown
            })
        ));
    }
}
