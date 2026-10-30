//! OpenAI Chat Completions wire types and conversions.
//!
//! The wire DTOs are deliberately strict: unknown fields are rejected instead
//! of silently ignored, because the gateway cannot honour parameters it does
//! not implement. The exception is the thinking family (`enable_thinking`,
//! `thinking`, `reasoning_effort`, budgets): clients set them from a single
//! toggle and the hint is forwarded only where the provider documents a
//! matching upstream field. All conversions go through the internal `protocol`
//! types; nothing here leaks into provider adapters.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::GatewayError;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, ToolCall,
    ToolChoice, ToolDefinition, ToolResult, Usage,
};

/// `POST /v1/chat/completions` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    /// Qoder-style thinking hints. Accepted so clients that always send them
    /// (Qoder IDE with 思考模式 on) keep working; budgets are not honoured.
    #[serde(default)]
    pub enable_thinking: Option<bool>,
    #[serde(default)]
    pub thinking: Option<Value>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub thinking_budget: Option<u64>,
    #[serde(default)]
    pub reasoning_budget_tokens: Option<u64>,
    #[serde(default)]
    pub preserve_thinking: Option<bool>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_completion_tokens: Option<u64>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop: Option<Stop>,
    #[serde(default)]
    pub tools: Option<Vec<WireTool>>,
    #[serde(default)]
    pub tool_choice: Option<WireToolChoice>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Stop {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    /// Assistant reasoning replayed by clients that keep the thinking trace
    /// (Qoder with `preserve_thinking`).
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<WireToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// Stream-only ordinal that some clients (Qoder) also replay in history;
    /// the array order is authoritative here, so it is accepted and ignored.
    #[serde(default)]
    pub index: Option<u32>,
    pub function: WireToolCallFunction,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireToolCallFunction {
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireFunction,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireFunction {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WireToolChoice {
    Mode(String),
    Function(WireToolChoiceFunction),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireToolChoiceFunction {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireToolChoiceName,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireToolChoiceName {
    pub name: String,
}

impl ChatCompletionRequest {
    pub fn streaming(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn include_usage(&self) -> bool {
        self.stream_options
            .as_ref()
            .map(|options| options.include_usage)
            .unwrap_or(false)
    }

    /// Resolve the several thinking spellings into one hint.
    fn thinking_hint(&self) -> Option<bool> {
        if let Some(thinking) = &self.thinking {
            match thinking.get("type").and_then(Value::as_str) {
                Some("disabled") => return Some(false),
                Some("enabled") | Some("adaptive") => return Some(true),
                _ => {}
            }
        }
        self.enable_thinking
    }

    /// Reject anything the gateway cannot honour instead of ignoring it.
    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.model.trim().is_empty() {
            return Err(GatewayError::bad_request("`model` must not be empty"));
        }
        if self.messages.is_empty() {
            return Err(GatewayError::bad_request("`messages` must not be empty"));
        }
        if self.n.unwrap_or(1) != 1 {
            return Err(GatewayError::bad_request(
                "`n` values other than 1 are not supported",
            ));
        }
        if self.max_tokens.is_some() && self.max_completion_tokens.is_some() {
            return Err(GatewayError::bad_request(
                "set either `max_tokens` or `max_completion_tokens`, not both",
            ));
        }
        if self.max_tokens == Some(0) || self.max_completion_tokens == Some(0) {
            return Err(GatewayError::bad_request(
                "token limits must be greater than zero",
            ));
        }
        if let Some(temperature) = self.temperature {
            if !(0.0..=2.0).contains(&temperature) {
                return Err(GatewayError::bad_request(
                    "`temperature` must be between 0 and 2",
                ));
            }
        }
        if let Some(top_p) = self.top_p {
            if !(0.0..=1.0).contains(&top_p) {
                return Err(GatewayError::bad_request("`top_p` must be between 0 and 1"));
            }
        }
        if self
            .reasoning_effort
            .as_deref()
            .is_some_and(|effort| effort.trim().is_empty())
        {
            return Err(GatewayError::bad_request(
                "`reasoning_effort` must not be empty",
            ));
        }
        if let Some(tools) = &self.tools {
            if tools.is_empty() {
                return Err(GatewayError::bad_request("`tools` must not be empty"));
            }
            for tool in tools {
                if tool.kind != "function" {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported tool type `{}`; only `function` is supported",
                        tool.kind
                    )));
                }
                if tool.function.name.trim().is_empty() {
                    return Err(GatewayError::bad_request("tool function name is required"));
                }
            }
        }
        if matches!(self.tool_choice, Some(WireToolChoice::Function(_)))
            && self.tools.as_ref().map(Vec::is_empty).unwrap_or(true)
        {
            return Err(GatewayError::bad_request(
                "`tool_choice` names a function but `tools` is empty",
            ));
        }
        Ok(())
    }

    /// Convert into the internal request. `upstream_model` is the provider's own
    /// model id resolved from the catalogue.
    pub fn to_chat_request(
        &self,
        upstream_model: &str,
        request_id: String,
    ) -> Result<ChatRequest, GatewayError> {
        let mut messages = Vec::with_capacity(self.messages.len());
        for message in &self.messages {
            messages.push(convert_message(message)?);
        }

        let tools = match &self.tools {
            Some(tools) => tools.iter().map(convert_tool).collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
        let tool_choice = match &self.tool_choice {
            Some(choice) => Some(convert_tool_choice(choice)?),
            None => None,
        };

        Ok(ChatRequest {
            request_id: Some(request_id),
            model: upstream_model.to_string(),
            messages,
            tools,
            tool_choice,
            max_output_tokens: self.max_completion_tokens.or(self.max_tokens),
            temperature: self.temperature,
            top_p: self.top_p,
            stop: self
                .stop
                .as_ref()
                .map(|stop| match stop {
                    Stop::One(one) => vec![one.clone()],
                    Stop::Many(many) => many.clone(),
                })
                .unwrap_or_default(),
            stream: self.streaming(),
            enable_thinking: self.thinking_hint(),
            reasoning_effort: self
                .reasoning_effort
                .as_ref()
                .map(|effort| effort.trim().to_string()),
            metadata: self.user.as_ref().map(|user| json!({ "user": user })),
        })
    }
}

fn parse_role(role: &str) -> Result<MessageRole, GatewayError> {
    match role {
        "system" => Ok(MessageRole::System),
        "developer" => Ok(MessageRole::Developer),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        other => Err(GatewayError::bad_request(format!(
            "unsupported message role `{other}`"
        ))),
    }
}

fn convert_message(message: &WireMessage) -> Result<ChatMessage, GatewayError> {
    let role = parse_role(&message.role)?;

    if role == MessageRole::Tool {
        if message.tool_calls.is_some() {
            return Err(GatewayError::bad_request(
                "tool messages must not carry `tool_calls`",
            ));
        }
        if message.reasoning_content.is_some() {
            return Err(GatewayError::bad_request(
                "`reasoning_content` is only valid on assistant messages",
            ));
        }
        let call_id = message
            .tool_call_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| GatewayError::bad_request("tool message requires `tool_call_id`"))?;
        let content = tool_content(message)?;
        return Ok(ChatMessage::new(
            MessageRole::Tool,
            vec![ContentBlock::ToolResult(ToolResult::new(call_id, content))],
        ));
    }

    if message.tool_call_id.is_some() {
        return Err(GatewayError::bad_request(
            "`tool_call_id` is only valid on tool messages",
        ));
    }

    let replay_reasoning = message
        .reasoning_content
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if replay_reasoning.is_some() && role != MessageRole::Assistant {
        return Err(GatewayError::bad_request(
            "`reasoning_content` is only valid on assistant messages",
        ));
    }

    let mut blocks = Vec::new();
    if let Some(reasoning) = replay_reasoning {
        blocks.push(ContentBlock::reasoning(reasoning.to_string()));
    }
    match &message.content {
        Some(Value::String(text)) => blocks.push(ContentBlock::text(text.clone())),
        Some(Value::Array(parts)) => {
            for part in parts {
                blocks.push(convert_part(part)?);
            }
        }
        Some(Value::Null) | None => {
            if role != MessageRole::Assistant {
                return Err(GatewayError::bad_request(format!(
                    "`{}` message requires content",
                    role.as_str()
                )));
            }
        }
        Some(_) => {
            return Err(GatewayError::bad_request(
                "message content must be a string or an array of parts",
            ))
        }
    }

    if let Some(calls) = &message.tool_calls {
        if role != MessageRole::Assistant {
            return Err(GatewayError::bad_request(
                "only assistant messages may carry `tool_calls`",
            ));
        }
        if calls.is_empty() {
            return Err(GatewayError::bad_request(
                "`tool_calls` must not be empty when present",
            ));
        }
        for call in calls {
            blocks.push(ContentBlock::ToolCall(convert_tool_call(call)?));
        }
    }

    let mut converted = ChatMessage::new(role, blocks);
    converted.name = message.name.clone();
    Ok(converted)
}

fn tool_content(message: &WireMessage) -> Result<String, GatewayError> {
    match &message.content {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                let object = part.as_object().ok_or_else(|| {
                    GatewayError::bad_request("tool content parts must be objects")
                })?;
                let kind = object
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::bad_request("content part requires `type`"))?;
                if kind != "text" {
                    return Err(GatewayError::bad_request(format!(
                        "tool content only supports text parts, got `{kind}`"
                    )));
                }
                let text = object
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::bad_request("text part requires `text`"))?;
                out.push_str(text);
            }
            Ok(out)
        }
        Some(Value::Null) | None => Err(GatewayError::bad_request(
            "tool message requires string content",
        )),
        Some(_) => Err(GatewayError::bad_request(
            "tool message content must be a string or text parts",
        )),
    }
}

fn convert_part(part: &Value) -> Result<ContentBlock, GatewayError> {
    let object = part
        .as_object()
        .ok_or_else(|| GatewayError::bad_request("content parts must be objects"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::bad_request("content part requires `type`"))?;
    if kind == "text" {
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| GatewayError::bad_request("text part requires `text`"))?;
        return Ok(ContentBlock::text(text));
    }
    // Preserve media the gateway cannot normalise instead of dropping it.
    Ok(ContentBlock::Unsupported {
        media_type: kind.to_string(),
        data: part.clone(),
    })
}

fn convert_tool_call(call: &WireToolCall) -> Result<ToolCall, GatewayError> {
    if call.kind != "function" {
        return Err(GatewayError::bad_request(format!(
            "unsupported tool call type `{}`; only `function` is supported",
            call.kind
        )));
    }
    if call.id.trim().is_empty() {
        return Err(GatewayError::bad_request(
            "tool call `id` must not be empty",
        ));
    }
    if call.function.name.trim().is_empty() {
        return Err(GatewayError::bad_request(
            "tool call function name must not be empty",
        ));
    }
    Ok(ToolCall::new(
        call.id.clone(),
        call.function.name.clone(),
        call.function.arguments.clone(),
    ))
}

fn convert_tool(tool: &WireTool) -> Result<ToolDefinition, GatewayError> {
    Ok(ToolDefinition {
        name: tool.function.name.clone(),
        description: tool.function.description.clone(),
        parameters: Some(
            tool.function
                .parameters
                .clone()
                .unwrap_or_else(|| json!({})),
        ),
    })
}

fn convert_tool_choice(choice: &WireToolChoice) -> Result<ToolChoice, GatewayError> {
    match choice {
        WireToolChoice::Mode(mode) => match mode.as_str() {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" => Ok(ToolChoice::Required),
            other => Err(GatewayError::bad_request(format!(
                "unsupported `tool_choice` value `{other}`"
            ))),
        },
        WireToolChoice::Function(function) => {
            if function.kind != "function" {
                return Err(GatewayError::bad_request(format!(
                    "unsupported tool_choice type `{}`",
                    function.kind
                )));
            }
            if function.function.name.trim().is_empty() {
                return Err(GatewayError::bad_request(
                    "tool_choice function name must not be empty",
                ));
            }
            Ok(ToolChoice::Tool(function.function.name.clone()))
        }
    }
}

// ---------------------------------------------------------------------------
// Response mapping
// ---------------------------------------------------------------------------

/// OpenAI `finish_reason` string. Internal reasons without a wire equivalent
/// (`Unknown`) fall back to `stop`; upstream `Other` values pass through.
pub fn finish_reason_name(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop | FinishReason::Cancelled | FinishReason::Error => "stop".into(),
        FinishReason::Length => "length".into(),
        FinishReason::ToolCalls => "tool_calls".into(),
        FinishReason::ContentFilter => "content_filter".into(),
        FinishReason::Unknown => "stop".into(),
        FinishReason::Other(other) if !other.trim().is_empty() => other.clone(),
        FinishReason::Other(_) => "stop".into(),
    }
}

pub fn usage_json(usage: Usage) -> Value {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total(),
    })
}

/// Non-streaming `chat.completion` body.
pub fn completion_json(response: &ChatResponse, exposed_model: &str, created: i64) -> Value {
    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));

    let text = response.message.text_content();
    message.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );

    let reasoning: String = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }

    let calls: Vec<Value> = response
        .message
        .tool_calls()
        .map(|call| {
            json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": call.arguments,
                }
            })
        })
        .collect();
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }

    json!({
        "id": format!("chatcmpl-{}", response.id),
        "object": "chat.completion",
        "created": created,
        "model": exposed_model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason_name(&response.finish_reason),
            "logprobs": Value::Null,
        }],
        "usage": response.usage.map(usage_json),
    })
}

impl super::wire::WireRequest for ChatCompletionRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn streaming(&self) -> bool {
        ChatCompletionRequest::streaming(self)
    }

    fn validate(&self) -> Result<(), GatewayError> {
        ChatCompletionRequest::validate(self)
    }

    fn route_needs(&self) -> crate::model_service::RouteNeeds {
        super::route_needs(self)
    }

    fn api_name(&self) -> &'static str {
        "openai.chat"
    }

    fn first_user_text(&self) -> Option<String> {
        let message = self
            .messages
            .iter()
            .find(|message| message.role == "user")?;
        let text = match message.content.as_ref()? {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<String>(),
            _ => return None,
        };
        (!text.trim().is_empty()).then_some(text)
    }

    fn to_chat_request(
        &self,
        upstream_model: &str,
        request_id: String,
    ) -> Result<ChatRequest, GatewayError> {
        ChatCompletionRequest::to_chat_request(self, upstream_model, request_id)
    }

    fn translator(&self, exposed_model: String) -> Box<dyn super::sse::SseTranslator> {
        Box::new(super::sse::SseState::new(
            exposed_model,
            self.include_usage(),
        ))
    }

    fn render(&self, response: &ChatResponse, exposed_model: &str) -> Result<Value, GatewayError> {
        Ok(completion_json(
            response,
            exposed_model,
            chrono::Utc::now().timestamp(),
        ))
    }
}
