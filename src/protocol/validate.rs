//! Explicit state machine validating [`StreamEvent`] order.
//!
//! Validation is structural: the state enum decides which event classes are
//! legal next, so no string comparison on event names is involved. A violation
//! is reported as [`ErrorKind::UpstreamServer`] because the event stream is
//! produced by an upstream adapter; the gateway itself must never emit an
//! invalid sequence.
//!
//! ```text
//! Fresh --Start--> Streaming --Usage--> AfterUsage --Finish--> Finished
//!   |                  |                   |
//!   +------Error-------+------Error--------+------Error-------> Failed
//! ```
//!
//! Allowed in `Streaming`: text, reasoning, tool call start, tool call argument
//! deltas, usage and finish. `AfterUsage` accepts only `Finish`/`Error`.
//! `Finished`/`Failed` accept nothing.

use std::collections::HashSet;

use crate::error::{ErrorKind, GatewayError};

use super::{StreamEvent, Usage};

/// Position of a stream inside the canonical protocol order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StreamState {
    /// No event seen yet.
    #[default]
    Fresh,
    /// `Start` seen, content events allowed.
    Streaming,
    /// The single `Usage` event was seen; only a terminator may follow.
    AfterUsage,
    /// `Finish` seen; the stream is over.
    Finished,
    /// `Error` seen; the stream is over.
    Failed,
}

impl StreamState {
    pub fn is_terminal(self) -> bool {
        matches!(self, StreamState::Finished | StreamState::Failed)
    }
}

/// Incremental validator; feed every event in order through [`validate`].
///
/// [`validate`]: StreamEventValidator::validate
#[derive(Debug, Clone, Default)]
pub struct StreamEventValidator {
    state: StreamState,
    tool_calls: HashSet<String>,
    response_id: Option<String>,
    usage: Option<Usage>,
}

impl StreamEventValidator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> StreamState {
        self.state
    }

    pub fn is_terminated(&self) -> bool {
        self.state.is_terminal()
    }

    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// Usage reported by the stream, if it carried one.
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    /// Validate one event and advance the state. On error the state is left
    /// unchanged, so a caller may stop and report the offending event.
    pub fn validate(&mut self, event: &StreamEvent) -> Result<(), GatewayError> {
        match self.state {
            StreamState::Finished => {
                return Err(violation("event received after the stream finished"));
            }
            StreamState::Failed => {
                return Err(violation("event received after the stream failed"));
            }
            _ => {}
        }

        match event {
            StreamEvent::Start {
                id,
                request_id: _,
                model: _,
            } => {
                if self.state != StreamState::Fresh {
                    return Err(violation("duplicate start event"));
                }
                if id.trim().is_empty() {
                    return Err(violation("start event requires a non-empty response id"));
                }
                self.response_id = Some(id.clone());
                self.state = StreamState::Streaming;
            }
            StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. } => {
                self.require_streaming("content")?;
            }
            StreamEvent::ToolCallStart { id, .. } => {
                self.require_streaming("tool call start")?;
                if id.trim().is_empty() {
                    return Err(violation("tool call start requires a non-empty id"));
                }
                if !self.tool_calls.insert(id.clone()) {
                    return Err(violation(format!(
                        "tool call id `{id}` was already used by another call"
                    )));
                }
            }
            StreamEvent::ToolCallArgsDelta { id, .. } => {
                self.require_streaming("tool call arguments")?;
                if !self.tool_calls.contains(id) {
                    return Err(violation(format!(
                        "tool call arguments for unknown id `{id}`"
                    )));
                }
            }
            StreamEvent::Usage { usage } => {
                self.require_streaming("usage")?;
                if self.usage.is_some() {
                    return Err(violation("duplicate usage event"));
                }
                self.usage = Some(*usage);
                self.state = StreamState::AfterUsage;
            }
            StreamEvent::Finish { .. } => {
                if self.state == StreamState::Fresh {
                    return Err(violation("finish event before start"));
                }
                self.state = StreamState::Finished;
            }
            StreamEvent::Error { .. } => {
                // An upstream failure may be reported before `Start` exists.
                self.state = StreamState::Failed;
            }
        }
        Ok(())
    }

    /// End-of-stream check: a stream that stops mid-flight is invalid.
    pub fn ensure_terminated(&self) -> Result<(), GatewayError> {
        match self.state {
            StreamState::Finished | StreamState::Failed => Ok(()),
            StreamState::Fresh => Err(violation("stream ended before start")),
            StreamState::Streaming | StreamState::AfterUsage => {
                Err(violation("stream ended without finish or error"))
            }
        }
    }

    fn require_streaming(&self, what: &str) -> Result<(), GatewayError> {
        match self.state {
            StreamState::Fresh => Err(violation(format!("{what} event before start"))),
            StreamState::AfterUsage => Err(violation(format!("{what} event after usage"))),
            StreamState::Streaming => Ok(()),
            StreamState::Finished | StreamState::Failed => {
                Err(violation(format!("{what} event after the stream ended")))
            }
        }
    }
}

fn violation(reason: impl std::fmt::Display) -> GatewayError {
    GatewayError::new(
        ErrorKind::UpstreamServer,
        format!("invalid stream event sequence: {reason}"),
    )
}
