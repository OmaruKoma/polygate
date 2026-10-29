//! Normalised chat response, usage and finish reasons.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::message::ChatMessage;

/// Why generation stopped.
///
/// `Other` keeps an unmapped upstream reason visible instead of silently
/// pretending it was `Stop`. Empty upstream strings map to `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
    Error,
    Unknown,
    Other(String),
}

impl FinishReason {
    /// Map a provider supplied reason onto the internal vocabulary.
    ///
    /// An empty string is never forwarded as-is: strict downstream clients
    /// reject `finish_reason: ""`, and the gateway must not invent `stop`
    /// either.
    pub fn from_upstream(raw: &str) -> Self {
        match raw.trim() {
            "" => FinishReason::Unknown,
            "stop" | "end_turn" | "stop_sequence" => FinishReason::Stop,
            "length" | "max_tokens" => FinishReason::Length,
            "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolCalls,
            "content_filter" | "content-filter" | "safety" => FinishReason::ContentFilter,
            "cancelled" | "canceled" => FinishReason::Cancelled,
            "error" => FinishReason::Error,
            other => FinishReason::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Cancelled => "cancelled",
            FinishReason::Error => "error",
            FinishReason::Unknown => "unknown",
            FinishReason::Other(other) => other,
        }
    }
}

/// Token accounting reported by an upstream.
///
/// `total_tokens` is optional because some providers omit it; use
/// [`Usage::total`] rather than summing the fields by hand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// Tokens spent on hidden reasoning (OpenAI `completion_tokens_details`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Input tokens served from a provider cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Upstream total, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl Usage {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            ..Self::default()
        }
    }

    /// Reported total when available, otherwise the sum of input and output.
    pub fn total(&self) -> u64 {
        self.total_tokens
            .unwrap_or_else(|| self.input_tokens.saturating_add(self.output_tokens))
    }
}

/// A completed, non-streamed chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Gateway generated response id.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub model: String,
    /// Final assistant message (text, reasoning, tool calls).
    pub message: ChatMessage,
    pub finish_reason: FinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub created_at: DateTime<Utc>,
}

impl ChatResponse {
    pub fn new(
        id: impl Into<String>,
        model: impl Into<String>,
        message: ChatMessage,
        finish_reason: FinishReason,
    ) -> Self {
        Self {
            id: id.into(),
            request_id: None,
            model: model.into(),
            message,
            finish_reason,
            usage: None,
            created_at: Utc::now(),
        }
    }

    /// Aggregate visible text, ignoring reasoning and tool calls.
    pub fn text(&self) -> String {
        self.message.text_content()
    }
}
