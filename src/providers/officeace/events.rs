//! OfficeAce SSE decoding and stream translation.
//!
//! The gateway emits standard OpenAI chunks, with one trap: rate limits and
//! upstream failures arrive as **HTTP 200** frames carrying an `error`
//! envelope, sometimes only after a healthy first frame. A 2xx is therefore
//! never taken as proof of a good stream; error frames become [`StreamEvent::Error`]
//! so the scheduler can cool the right scope.
//!
//! This module is pure: no HTTP, no provider state.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{FinishReason, StreamEvent};

use super::convert::parse_usage;

pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Incremental `data:` frame parser. Bytes may split anywhere, including inside
/// a multi-byte UTF-8 character.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: String,
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
            return Err(GatewayError::new(
                ErrorKind::UpstreamServer,
                "OfficeAce SSE line exceeded the gateway limit",
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
            GatewayError::new(
                ErrorKind::UpstreamServer,
                "OfficeAce returned invalid UTF-8 in its stream",
            )
        })?;
        if line.is_empty() {
            return Ok(self.flush());
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest);
        }
        Ok(None)
    }

    fn flush(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.data))
    }
}

/// Parsed frame: either a chunk or the terminal marker.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Data(Value),
    Done,
}

pub fn parse_frame(frame: &str) -> Result<Frame, GatewayError> {
    let frame = frame.trim();
    if frame == "[DONE]" {
        return Ok(Frame::Done);
    }
    let value: Value = serde_json::from_str(frame).map_err(|_| {
        GatewayError::new(
            ErrorKind::UpstreamServer,
            "OfficeAce sent a stream frame that is not JSON",
        )
    })?;
    Ok(Frame::Data(value))
}

/// Maps OpenAI-shaped chunks onto [`StreamEvent`]s.
#[derive(Default)]
pub struct Translator {
    started: bool,
    finished: bool,
    finish: Option<FinishReason>,
    tool_ids: BTreeMap<i64, (String, bool)>,
    response_id: String,
    model: String,
}

impl Translator {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, value: &Value) -> Vec<StreamEvent> {
        if self.finished {
            return Vec::new();
        }
        if let Some(error) = error_event(value) {
            self.finished = true;
            return vec![error];
        }

        let mut events = Vec::new();
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            if self.response_id.is_empty() && !id.is_empty() {
                self.response_id = id.to_string();
            }
        }
        if !self.started {
            let model = value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| self.model.clone());
            let id = if self.response_id.is_empty() {
                format!("officeace-{}", uuid::Uuid::new_v4())
            } else {
                self.response_id.clone()
            };
            events.push(StreamEvent::Start {
                id,
                request_id: None,
                model,
            });
            self.started = true;
        }

        if let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
            if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
                if !reasoning.is_empty() {
                    events.push(StreamEvent::ReasoningDelta {
                        text: reasoning.to_string(),
                    });
                }
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(StreamEvent::TextDelta {
                        text: text.to_string(),
                    });
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_i64).unwrap_or(0);
                    let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                    let function = call.get("function").cloned().unwrap_or(Value::Null);
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let entry = self
                        .tool_ids
                        .entry(index)
                        .or_insert_with(|| (String::new(), false));
                    if entry.0.is_empty() && !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                    if !entry.1 && !entry.0.is_empty() && !name.is_empty() {
                        entry.1 = true;
                        events.push(StreamEvent::ToolCallStart {
                            id: entry.0.clone(),
                            name: name.to_string(),
                            index: Some(index as u32),
                        });
                    }
                    if !arguments.is_empty() {
                        let id = if entry.0.is_empty() {
                            format!("call_{index}")
                        } else {
                            entry.0.clone()
                        };
                        events.push(StreamEvent::ToolCallArgsDelta {
                            id,
                            delta: arguments.to_string(),
                        });
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !reason.trim().is_empty() {
                    self.finish = Some(FinishReason::from_upstream(reason));
                }
            }
        }

        if let Some(usage) = value.get("usage").filter(|value| value.is_object()) {
            events.push(StreamEvent::Usage {
                usage: parse_usage(usage),
            });
        }
        events
    }

    /// Terminal event. A stream that never reported a finish reason is reported
    /// as [`FinishReason::Unknown`], never invented as `stop`.
    pub fn finish(&mut self) -> Option<StreamEvent> {
        if self.finished {
            return None;
        }
        self.finished = true;
        Some(StreamEvent::Finish {
            reason: self.finish.clone().unwrap_or(FinishReason::Unknown),
        })
    }
}

/// Upstream error envelopes: `{"error": {...}}`, `{"error": "..."}` or a bare
/// `{"code": ..., "message": ...}` with a business code.
fn error_event(value: &Value) -> Option<StreamEvent> {
    let error = value.get("error")?;
    let raw_code = error
        .get("code")
        .cloned()
        .or_else(|| value.get("code").cloned());
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("OfficeAce stream error");
    let mut text = message.to_string();
    if let Some(code) = &raw_code {
        if let Some(code) = code.as_str() {
            if !code.trim().is_empty() {
                text = format!("{code} {text}");
            }
        } else if let Some(code) = code.as_i64() {
            text = format!("{code} {text}");
        }
    }
    Some(StreamEvent::Error {
        error: crate::protocol::StreamError::new(classify_stream_error(&text), text),
    })
}

/// Business codes first: `81004`/`81009` arrive on a 200 frame but only mean
/// "this account cannot use this model".
pub fn classify_stream_error(text: &str) -> ErrorKind {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("81004") || lowered.contains("81009") {
        ErrorKind::ModelUnavailable
    } else if ["81111", "81112", "81114"]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        ErrorKind::RateLimited
    } else if lowered.contains("81113") || lowered.contains("81001") {
        ErrorKind::BadRequest
    } else if lowered.contains("insufficient balance")
        || lowered.contains("余额不足")
        || lowered.contains("14018")
    {
        ErrorKind::QuotaExhausted
    } else if lowered.contains("apig.1009")
        || lowered.contains("apikey")
        || lowered.contains("apig.0303")
    {
        ErrorKind::AuthInvalid
    } else {
        ErrorKind::UpstreamServer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_decoder_reassembles_frames_split_across_chunks() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push(b"data: {\"a\":").expect("push").is_empty());
        let frames = decoder.push(b"1}\n\n").expect("push");
        assert_eq!(frames, vec!["{\"a\":1}".to_string()]);
    }

    #[test]
    fn a_done_frame_is_recognised() {
        assert_eq!(parse_frame("[DONE]").expect("done"), Frame::Done);
    }

    #[test]
    fn a_fake_200_error_frame_becomes_a_typed_error_event() {
        let mut translator = Translator::new("m");
        translator.push(&json!({"choices": [{"delta": {"content": "a"}}]}));
        let events = translator.push(&json!({
            "error": {"code": "ModelArts.81004", "message": "no permission"}
        }));
        match events.as_slice() {
            [StreamEvent::Error { error }] => {
                assert_eq!(error.kind, ErrorKind::ModelUnavailable);
            }
            other => panic!("unexpected events: {other:?}"),
        }
        assert!(translator.finish().is_none(), "an error ends the stream");
    }

    #[test]
    fn chunks_become_start_text_reasoning_tool_and_finish_events() {
        let mut translator = Translator::new("m");
        let events = translator.push(&json!({
            "id": "r1", "model": "m",
            "choices": [{"delta": {"reasoning_content": "think"}, "finish_reason": null}]
        }));
        assert!(matches!(events[0], StreamEvent::Start { .. }));
        assert!(matches!(events[1], StreamEvent::ReasoningDelta { .. }));

        let events = translator.push(&json!({
            "choices": [{"delta": {"content": "hi"}, "finish_reason": null}]
        }));
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));

        let events = translator.push(&json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": "call_1",
                "function": {"name": "shell", "arguments": "{\"a\":"}
            }]}, "finish_reason": null}]
        }));
        assert!(matches!(events[0], StreamEvent::ToolCallStart { .. }));
        assert!(matches!(events[1], StreamEvent::ToolCallArgsDelta { .. }));

        translator.push(&json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}));
        assert_eq!(
            translator.finish(),
            Some(StreamEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        );
        assert!(translator.finish().is_none());
    }
}
