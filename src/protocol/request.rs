//! Normalised chat request.

use serde::{Deserialize, Serialize};

use super::message::{ChatMessage, ToolDefinition};

/// How the model should decide on tool use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    /// Force one specific tool by name.
    Tool(String),
}

/// A provider independent chat completion request.
///
/// Optional knobs are `None` when the downstream client did not set them, so a
/// provider adapter can apply its own default instead of receiving a fabricated
/// gateway default. `stream` only selects the delivery mode; the payload model
/// is the same for [`crate::protocol::StreamEvent`] streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Downstream correlation id (gateway request / session scoped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Upper bound for generated tokens. Providers that call it `max_tokens`
    /// or `max_completion_tokens` translate at their adapter boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    /// Opaque caller metadata. Adapters must not forward it upstream unless the
    /// provider documents a matching field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            request_id: None,
            model: model.into(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stream: false,
            metadata: None,
        }
    }
}
