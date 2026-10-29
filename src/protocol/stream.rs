//! Normalised streaming events.
//!
//! Canonical order (see `docs/ARCHITECTURE.md` §8):
//!
//! ```text
//! Start -> TextDelta* / ReasoningDelta* / ToolCallStart*
//!       -> ToolCallArgsDelta* -> Usage -> Finish | Error
//! ```
//!
//! `Usage` is optional, at most once, and once emitted only `Finish` or `Error`
//! may follow. Tool call argument fragments are matched by [`ToolCall::id`].

use serde::{Deserialize, Serialize};

use super::response::{FinishReason, Usage};
use crate::error::{ErrorKind, GatewayError};

/// A normalised stream event. The wire tag is `event`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    /// The upstream accepted the request and assigned a response id.
    Start {
        /// Gateway wide response id; stable for the whole stream.
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        model: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// A tool invocation started. `index` preserves upstream ordering when it
    /// is reported; `id` is the key clients must use for fragments.
    ToolCallStart {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
    },
    /// A fragment of the JSON arguments for the call identified by `id`.
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    Usage {
        usage: Usage,
    },
    Finish {
        reason: FinishReason,
    },
    /// Terminal abnormal end. The body is a client-safe projection: no
    /// diagnostic detail, no upstream headers, no credentials.
    Error {
        error: StreamError,
    },
}

impl StreamEvent {
    pub fn start(
        id: impl Into<String>,
        request_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        StreamEvent::Start {
            id: id.into(),
            request_id,
            model: model.into(),
        }
    }

    pub fn text_delta(text: impl Into<String>) -> Self {
        StreamEvent::TextDelta { text: text.into() }
    }

    pub fn reasoning_delta(text: impl Into<String>) -> Self {
        StreamEvent::ReasoningDelta { text: text.into() }
    }

    pub fn tool_call_start(id: impl Into<String>, name: impl Into<String>) -> Self {
        StreamEvent::ToolCallStart {
            id: id.into(),
            name: name.into(),
            index: None,
        }
    }

    pub fn tool_call_args_delta(id: impl Into<String>, delta: impl Into<String>) -> Self {
        StreamEvent::ToolCallArgsDelta {
            id: id.into(),
            delta: delta.into(),
        }
    }

    pub fn usage(usage: Usage) -> Self {
        StreamEvent::Usage { usage }
    }

    pub fn finish(reason: FinishReason) -> Self {
        StreamEvent::Finish { reason }
    }

    pub fn error(error: StreamError) -> Self {
        StreamEvent::Error { error }
    }

    /// Whether the event ends the stream (normal or abnormal).
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamEvent::Finish { .. } | StreamEvent::Error { .. })
    }

    /// Whether the event carries generated content or its tool metadata.
    pub fn is_content(&self) -> bool {
        matches!(
            self,
            StreamEvent::TextDelta { .. }
                | StreamEvent::ReasoningDelta { .. }
                | StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgsDelta { .. }
        )
    }
}

/// Client-safe error projection carried by [`StreamEvent::Error`].
///
/// Built from [`GatewayError::public_view`] semantics: the diagnostic
/// `detail`, upstream status and headers never enter the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

impl StreamError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: crate::error::redact_secrets(&message.into()),
            retry_after: None,
        }
    }

    pub fn from_gateway_error(err: &GatewayError) -> Self {
        Self {
            kind: err.kind,
            message: crate::error::redact_secrets(&err.message),
            retry_after: err.retry_after,
        }
    }
}
