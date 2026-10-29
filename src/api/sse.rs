//! `StreamEvent` → OpenAI `chat.completion.chunk` SSE translation.
//!
//! The internal event stream is never forwarded verbatim: each event becomes
//! zero or more OpenAI chunks, terminated by `data: [DONE]`. Tool call index
//! bookkeeping happens here so downstream clients can interleave argument
//! fragments across calls.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;

use axum::response::sse::Event;
use chrono::Utc;
use futures_util::{stream, Stream, StreamExt};
use serde_json::{json, Value};

use crate::error::{openai_error_body, GatewayError};
use crate::protocol::{StreamEvent, Usage};
use crate::providers::ChatStream;

use super::openai::{finish_reason_name, usage_json};

/// Anything that can translate internal stream events into SSE frames.
///
/// One implementation exists per wire API (OpenAI chat, Responses, Anthropic
/// messages). The terminal methods are called exactly once: `end` when the
/// provider stream ends without a terminal event, `fail` on transport errors.
pub trait SseTranslator: Send + 'static {
    fn handle(&mut self, event: StreamEvent) -> Vec<Event>;
    fn end(&mut self) -> Vec<Event>;
    fn fail(&mut self, err: &GatewayError) -> Vec<Event>;
}

impl SseTranslator for Box<dyn SseTranslator> {
    fn handle(&mut self, event: StreamEvent) -> Vec<Event> {
        (**self).handle(event)
    }
    fn end(&mut self) -> Vec<Event> {
        (**self).end()
    }
    fn fail(&mut self, err: &GatewayError) -> Vec<Event> {
        (**self).fail(err)
    }
}

/// Drive any [`SseTranslator`] over a provider stream. The returned stream owns
/// the provider stream, so dropping the HTTP body (client disconnect) drops the
/// provider stream and cancels the upstream work.
pub fn to_sse<S>(
    inner: ChatStream,
    state: S,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static
where
    S: SseTranslator,
{
    let session = SseSession {
        inner,
        state,
        queue: VecDeque::new(),
        done: false,
    };
    stream::unfold(session, |mut session| async move {
        session.next_frame().await.map(|frame| (Ok(frame), session))
    })
}

struct SseSession<S> {
    inner: ChatStream,
    state: S,
    queue: VecDeque<Event>,
    done: bool,
}

impl<S: SseTranslator> SseSession<S> {
    async fn next_frame(&mut self) -> Option<Event> {
        loop {
            if let Some(frame) = self.queue.pop_front() {
                return Some(frame);
            }
            if self.done {
                return None;
            }
            match self.inner.next().await {
                Some(Ok(event)) => self.queue.extend(self.state.handle(event)),
                Some(Err(err)) => {
                    self.done = true;
                    self.queue.extend(self.state.fail(&err));
                }
                None => {
                    self.done = true;
                    self.queue.extend(self.state.end());
                }
            }
        }
    }
}

/// Per-stream state: response id, model, tool call indices, usage.
pub struct SseState {
    id: Option<String>,
    model: String,
    created: i64,
    include_usage: bool,
    tool_indices: HashMap<String, u32>,
    next_index: u32,
    usage: Option<Usage>,
    terminal: bool,
}

impl SseState {
    pub fn new(model: impl Into<String>, include_usage: bool) -> Self {
        Self {
            id: None,
            model: model.into(),
            created: Utc::now().timestamp(),
            include_usage,
            tool_indices: HashMap::new(),
            next_index: 0,
            usage: None,
            terminal: false,
        }
    }

    pub fn handle(&mut self, event: StreamEvent) -> Vec<Event> {
        match event {
            StreamEvent::Start { id, .. } => {
                self.id = Some(id);
                vec![self.chunk(json!({ "role": "assistant", "content": "" }), None)]
            }
            StreamEvent::TextDelta { text } => vec![self.chunk(json!({ "content": text }), None)],
            StreamEvent::ReasoningDelta { text } => {
                vec![self.chunk(json!({ "reasoning_content": text }), None)]
            }
            StreamEvent::ToolCallStart { id, name, index } => {
                let index = self.assign_index(&id, index);
                vec![self.chunk(
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" }
                        }]
                    }),
                    None,
                )]
            }
            StreamEvent::ToolCallArgsDelta { id, delta } => {
                let index = self.assign_index(&id, None);
                vec![self.chunk(
                    json!({
                        "tool_calls": [{
                            "index": index,
                            "function": { "arguments": delta }
                        }]
                    }),
                    None,
                )]
            }
            StreamEvent::Usage { usage } => {
                self.usage = Some(usage);
                Vec::new()
            }
            StreamEvent::Finish { reason } => {
                let mut frames = vec![self.chunk(json!({}), Some(finish_reason_name(&reason)))];
                if let Some(usage) = self.usage_chunk() {
                    frames.push(usage);
                }
                frames.push(done_event());
                self.terminal = true;
                frames
            }
            StreamEvent::Error { error } => {
                self.terminal = true;
                let err = GatewayError::new(error.kind, error.message);
                vec![error_event(&err), done_event()]
            }
        }
    }

    /// Called when the provider stream ends without a terminal event.
    pub fn end(&mut self) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        self.terminal = true;
        let err = GatewayError::upstream_server("upstream stream ended without a terminal event");
        vec![error_event(&err), done_event()]
    }

    /// Called with a transport/upstream error in the middle of the stream.
    pub fn fail(&mut self, err: &GatewayError) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        self.terminal = true;
        vec![error_event(err), done_event()]
    }

    fn assign_index(&mut self, id: &str, hint: Option<u32>) -> u32 {
        if let Some(index) = self.tool_indices.get(id) {
            return *index;
        }
        let index = hint.unwrap_or(self.next_index);
        self.next_index = self.next_index.max(index + 1);
        self.tool_indices.insert(id.to_string(), index);
        index
    }

    fn chunk(&self, delta: Value, finish_reason: Option<String>) -> Event {
        let id = self
            .id
            .as_ref()
            .map(|id| format!("chatcmpl-{id}"))
            .unwrap_or_else(|| "chatcmpl-polygate".to_string());
        let body = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
                "logprobs": Value::Null,
            }],
            "usage": Value::Null,
        });
        Event::default().data(body.to_string())
    }

    fn usage_chunk(&self) -> Option<Event> {
        if !self.include_usage {
            return None;
        }
        let usage = self.usage?;
        let body = json!({
            "id": self.id.as_ref().map(|id| format!("chatcmpl-{id}")).unwrap_or_else(|| "chatcmpl-polygate".to_string()),
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": usage_json(usage),
        });
        Some(Event::default().data(body.to_string()))
    }
}

impl SseTranslator for SseState {
    fn handle(&mut self, event: StreamEvent) -> Vec<Event> {
        SseState::handle(self, event)
    }

    fn end(&mut self) -> Vec<Event> {
        SseState::end(self)
    }

    fn fail(&mut self, err: &GatewayError) -> Vec<Event> {
        SseState::fail(self, err)
    }
}

fn done_event() -> Event {
    Event::default().data("[DONE]")
}

fn error_event(err: &GatewayError) -> Event {
    Event::default().data(openai_error_body(err).to_string())
}
