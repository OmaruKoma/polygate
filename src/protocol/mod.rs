//! Provider independent protocol model.
//!
//! This module is the gateway's internal language: requests, responses and
//! streaming events that every provider adapter translates to and from. It is
//! deliberately separate from the OpenAI/Anthropic wire formats; those DTOs
//! live in the API layer and must not leak into provider code.
//!
//! Serialization policy:
//! * All types derive [`serde::Serialize`] / [`serde::Deserialize`].
//! * Absent optional fields are omitted on the wire (`skip_serializing_if`).
//! * Unknown JSON fields are ignored on deserialization. Internal types are
//!   forward compatible; external adapter DTOs are responsible for rejecting
//!   input they cannot map.
//! * Unknown provider content is represented by [`ContentBlock::Unsupported`],
//!   and unknown finish reasons by [`FinishReason::Other`] — never discarded.
//!
//! Event order is enforced by [`StreamEventValidator`].

mod message;
mod request;
mod response;
mod stream;
mod validate;

#[cfg(test)]
mod tests;

pub use message::{ChatMessage, ContentBlock, MessageRole, ToolCall, ToolDefinition, ToolResult};
pub use request::{ChatRequest, ToolChoice};
pub use response::{ChatResponse, FinishReason, Usage};
pub use stream::{StreamError, StreamEvent};
pub use validate::{StreamEventValidator, StreamState};
