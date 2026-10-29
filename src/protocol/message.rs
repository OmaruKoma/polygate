//! Provider independent message model.
//!
//! The gateway normalises every upstream into these types. Anything a provider
//! cannot express maps to [`ContentBlock::Unsupported`] explicitly instead of
//! being dropped, so adapters can report the loss to the caller.

use serde::{Deserialize, Serialize};

/// Conversation role. `Developer` is kept distinct from `System` because newer
/// OpenAI style clients use it for mid-conversation instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageRole::System => "system",
            MessageRole::Developer => "developer",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        }
    }

    /// Roles that carry operator instructions rather than conversation turns.
    pub fn is_instruction(self) -> bool {
        matches!(self, MessageRole::System | MessageRole::Developer)
    }
}

/// One content block inside a message.
///
/// The tag is `type`, matching the shape both OpenAI and Anthropic use for
/// content arrays, but the field names stay gateway-internal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Provider emitted reasoning / thinking content. Always optional in a
    /// response; never required from a request.
    Reasoning {
        text: String,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    /// A block this gateway cannot represent yet (image, audio, provider
    /// specific attachment, ...). Kept verbatim so the adapter can decide
    /// whether to forward, reject, or report it.
    Unsupported {
        media_type: String,
        data: serde_json::Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        ContentBlock::Reasoning { text: text.into() }
    }
}

/// A model requested tool invocation.
///
/// `arguments` is the raw JSON string, not a parsed value: streamed argument
/// fragments are concatenated first, and an invalid final string must stay
/// observable rather than panic during deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Stable id assigned upstream (or by the gateway for local tools).
    pub id: String,
    pub name: String,
    /// Raw JSON object string. May be empty or, mid-stream, incomplete.
    #[serde(default)]
    pub arguments: String,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    pub fn parse_arguments(&self) -> Result<serde_json::Value, crate::error::GatewayError> {
        if self.arguments.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        serde_json::from_str(&self.arguments).map_err(|e| {
            // The raw arguments are model output and may contain user data, so
            // only the parse error and the call id are reported.
            crate::error::GatewayError::upstream_server(format!(
                "tool call `{}` returned invalid JSON arguments",
                self.id
            ))
            .with_detail(e.to_string())
        })
    }
}

/// Result of executing a [`ToolCall`], produced by the downstream client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResult {
    pub fn new(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// A tool the model may call. `parameters` is an unmodified JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            parameters: Some(parameters),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// One turn of the conversation.
///
/// A tool result is carried by a `tool` role message as a
/// [`ContentBlock::ToolResult`]; providers that use a separate tool result
/// message shape are mapped by their adapter, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Optional participant name (OpenAI `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn new(role: MessageRole, content: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content,
            name: None,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::text_message(MessageRole::System, text)
    }

    pub fn developer(text: impl Into<String>) -> Self {
        Self::text_message(MessageRole::Developer, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text_message(MessageRole::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text_message(MessageRole::Assistant, text)
    }

    pub fn tool(result: ToolResult) -> Self {
        Self::new(MessageRole::Tool, vec![ContentBlock::ToolResult(result)])
    }

    fn text_message(role: MessageRole, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentBlock::text(text)])
    }

    /// Concatenated text blocks. Reasoning, tool calls and unsupported blocks
    /// are intentionally excluded.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    pub fn tool_results(&self) -> impl Iterator<Item = &ToolResult> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolResult(result) => Some(result),
            _ => None,
        })
    }

    /// Blocks that the gateway cannot forward to a given provider verbatim.
    pub fn unsupported_blocks(&self) -> impl Iterator<Item = &ContentBlock> {
        self.content
            .iter()
            .filter(|b| matches!(b, ContentBlock::Unsupported { .. }))
    }
}
