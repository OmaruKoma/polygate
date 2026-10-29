//! Anthropic Messages API (`POST /v1/messages`).
//!
//! A third wire surface over the internal protocol. Supported: `system`,
//! multi-turn `messages`, text and image content, `tool_use` / `tool_result`,
//! function tools, streaming message events, stop reasons and Anthropic error
//! bodies. Fields with no internal equivalent (`top_k`, thinking blocks) are
//! rejected instead of being silently dropped.

use std::collections::HashMap;

use axum::response::sse::Event;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{anthropic_error_body, GatewayError};
use crate::model_service::RouteNeeds;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, ToolDefinition, ToolResult, Usage,
};

use super::sse::SseTranslator;
use super::wire::{block_has_image, WireRequest};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u64,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<Value>,
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub tool_choice: Option<AnthropicToolChoice>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolChoice {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
}

fn text_part(part: &Value) -> Result<String, GatewayError> {
    part.get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GatewayError::bad_request("text block requires `text`"))
}

fn system_blocks(system: &Value) -> Result<Vec<ContentBlock>, GatewayError> {
    match system {
        Value::String(text) => Ok(vec![ContentBlock::text(text.clone())]),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::bad_request("system block requires `type`"))?;
                if kind != "text" {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported system block `{kind}`"
                    )));
                }
                blocks.push(ContentBlock::text(text_part(part)?));
            }
            Ok(blocks)
        }
        _ => Err(GatewayError::bad_request(
            "`system` must be a string or an array of text blocks",
        )),
    }
}

fn tool_result_blocks(content: &Value) -> Result<String, GatewayError> {
    match content {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
                if kind != "text" {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported tool_result content block `{kind}`"
                    )));
                }
                out.push_str(&text_part(part)?);
            }
            Ok(out)
        }
        Value::Null => Ok(String::new()),
        _ => Err(GatewayError::bad_request(
            "tool_result `content` must be a string or text blocks",
        )),
    }
}

fn convert_blocks(
    role: MessageRole,
    content: &Value,
) -> Result<(Vec<ContentBlock>, Vec<ToolResult>), GatewayError> {
    if let Value::String(text) = content {
        return Ok((vec![ContentBlock::text(text.clone())], Vec::new()));
    }
    let parts = content
        .as_array()
        .ok_or_else(|| GatewayError::bad_request("message `content` must be a string or array"))?;
    let mut blocks = Vec::new();
    let mut results = Vec::new();
    for part in parts {
        let kind = part
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| GatewayError::bad_request("content block requires `type`"))?;
        match kind {
            "text" => blocks.push(ContentBlock::text(text_part(part)?)),
            "image" => blocks.push(ContentBlock::Unsupported {
                media_type: "image".into(),
                data: part.clone(),
            }),
            "tool_use" => {
                if role != MessageRole::Assistant {
                    return Err(GatewayError::bad_request(
                        "`tool_use` blocks are only valid in assistant messages",
                    ));
                }
                let id = part
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| GatewayError::bad_request("tool_use requires `id`"))?;
                let name = part
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| GatewayError::bad_request("tool_use requires `name`"))?;
                let input = part.get("input").cloned().unwrap_or_else(|| json!({}));
                if !input.is_object() {
                    return Err(GatewayError::bad_request(
                        "tool_use `input` must be an object",
                    ));
                }
                blocks.push(ContentBlock::ToolCall(ToolCall::new(
                    id,
                    name,
                    input.to_string(),
                )));
            }
            "tool_result" => {
                if role != MessageRole::User {
                    return Err(GatewayError::bad_request(
                        "`tool_result` blocks are only valid in user messages",
                    ));
                }
                let call_id = part
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        GatewayError::bad_request("tool_result requires `tool_use_id`")
                    })?;
                let content = part.get("content").cloned().unwrap_or(Value::Null);
                let is_error = part
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                results.push(ToolResult {
                    tool_call_id: call_id.to_string(),
                    content: tool_result_blocks(&content)?,
                    is_error,
                });
            }
            "thinking" | "redacted_thinking" => {
                return Err(GatewayError::bad_request(
                    "extended thinking blocks are not supported",
                ))
            }
            other => {
                return Err(GatewayError::bad_request(format!(
                    "unsupported content block `{other}`"
                )))
            }
        }
    }
    Ok((blocks, results))
}

fn stop_reason(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "end_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::ToolCalls => "tool_use",
        FinishReason::ContentFilter => "refusal",
        _ => "end_turn",
    }
}

impl AnthropicRequest {
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .map(|tool| ToolDefinition {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: Some(
                            tool.input_schema
                                .clone()
                                .unwrap_or_else(|| json!({"type": "object"})),
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl WireRequest for AnthropicRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn streaming(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    fn validate(&self) -> Result<(), GatewayError> {
        if self.model.trim().is_empty() {
            return Err(GatewayError::bad_request("`model` must not be empty"));
        }
        if self.messages.is_empty() {
            return Err(GatewayError::bad_request("`messages` must not be empty"));
        }
        if self.max_tokens == 0 {
            return Err(GatewayError::bad_request(
                "`max_tokens` must be greater than zero",
            ));
        }
        if self.top_k.is_some() {
            return Err(GatewayError::bad_request(
                "`top_k` is not supported by this gateway",
            ));
        }
        if let Some(temperature) = self.temperature {
            if !(0.0..=1.0).contains(&temperature) {
                return Err(GatewayError::bad_request(
                    "`temperature` must be between 0 and 1",
                ));
            }
        }
        if let Some(top_p) = self.top_p {
            if !(0.0..=1.0).contains(&top_p) {
                return Err(GatewayError::bad_request("`top_p` must be between 0 and 1"));
            }
        }
        for message in &self.messages {
            if message.role != "user" && message.role != "assistant" {
                return Err(GatewayError::bad_request(format!(
                    "unsupported message role `{}`; use `user` or `assistant`",
                    message.role
                )));
            }
        }
        if let Some(system) = &self.system {
            system_blocks(system)?;
        }
        if let Some(tools) = &self.tools {
            if tools.is_empty() {
                return Err(GatewayError::bad_request("`tools` must not be empty"));
            }
            for tool in tools {
                if tool.name.trim().is_empty() {
                    return Err(GatewayError::bad_request("tool `name` is required"));
                }
            }
        }
        if let Some(choice) = &self.tool_choice {
            match choice.kind.as_str() {
                "auto" | "none" | "any" => {}
                "tool" => {
                    let name = choice
                        .name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            GatewayError::bad_request("tool_choice `tool` requires `name`")
                        })?;
                    if !self
                        .tools
                        .as_ref()
                        .is_some_and(|tools| tools.iter().any(|tool| tool.name == name))
                    {
                        return Err(GatewayError::bad_request(
                            "tool_choice names a tool that is not in `tools`",
                        ));
                    }
                }
                other => {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported tool_choice type `{other}`"
                    )))
                }
            }
        }
        Ok(())
    }

    fn route_needs(&self) -> RouteNeeds {
        let tools = self
            .tools
            .as_ref()
            .map(|tools| !tools.is_empty())
            .unwrap_or(false)
            || self.tool_choice.is_some();
        let mut vision = false;
        for message in &self.messages {
            if let Value::Array(parts) = &message.content {
                if parts.iter().any(block_has_image) {
                    vision = true;
                }
            }
        }
        RouteNeeds {
            streaming: self.streaming(),
            tools,
            vision,
        }
    }

    fn api_name(&self) -> &'static str {
        "anthropic.messages"
    }

    fn first_user_text(&self) -> Option<String> {
        for message in &self.messages {
            if message.role != "user" {
                continue;
            }
            let text = match &message.content {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>(),
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
        None
    }

    fn to_chat_request(
        &self,
        upstream_model: &str,
        request_id: String,
    ) -> Result<ChatRequest, GatewayError> {
        let mut messages = Vec::new();
        if let Some(system) = &self.system {
            let blocks = system_blocks(system)?;
            if !blocks.is_empty() {
                messages.push(ChatMessage::new(MessageRole::System, blocks));
            }
        }
        for message in &self.messages {
            let role = if message.role == "assistant" {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let (blocks, results) = convert_blocks(role, &message.content)?;
            // A user turn may interleave tool results with text. The internal
            // model has a dedicated tool role, so results are emitted first and
            // the remaining visible text follows as the user turn.
            for result in results {
                messages.push(ChatMessage::tool(result));
            }
            if !blocks.is_empty() || message.content.as_array().is_some_and(Vec::is_empty) {
                messages.push(ChatMessage::new(role, blocks));
            }
        }
        if messages.is_empty() {
            return Err(GatewayError::bad_request("`messages` produced no turns"));
        }
        let tool_choice = self
            .tool_choice
            .as_ref()
            .map(|choice| match choice.kind.as_str() {
                "auto" => ToolChoice::Auto,
                "none" => ToolChoice::None,
                "any" => ToolChoice::Required,
                "tool" => ToolChoice::Tool(choice.name.clone().unwrap_or_default()),
                _ => ToolChoice::Auto,
            });
        Ok(ChatRequest {
            request_id: Some(request_id),
            model: upstream_model.to_string(),
            messages,
            tools: self.tool_definitions(),
            tool_choice,
            max_output_tokens: Some(self.max_tokens),
            temperature: self.temperature,
            top_p: self.top_p,
            stop: self.stop_sequences.clone().unwrap_or_default(),
            stream: self.streaming(),
            metadata: self.metadata.clone(),
        })
    }

    fn translator(&self, exposed_model: String) -> Box<dyn SseTranslator> {
        Box::new(AnthropicStream::new(exposed_model))
    }

    fn render(&self, response: &ChatResponse, exposed_model: &str) -> Result<Value, GatewayError> {
        let mut content = Vec::new();
        for block in &response.message.content {
            match block {
                ContentBlock::Text { text } => {
                    if !text.is_empty() {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                }
                ContentBlock::ToolCall(call) => {
                    let input = call.parse_arguments()?;
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input,
                    }));
                }
                // The Messages API has no visible reasoning block unless
                // thinking was requested; it was not, so it is not re-labelled
                // as text.
                ContentBlock::Reasoning { .. } => {}
                ContentBlock::ToolResult(_) => {}
                ContentBlock::Unsupported { .. } => {}
            }
        }
        Ok(json!({
            "id": format!("msg-{}", response.id),
            "type": "message",
            "role": "assistant",
            "model": exposed_model,
            "content": content,
            "stop_reason": stop_reason(&response.finish_reason),
            "stop_sequence": Value::Null,
            "usage": response.usage.map(|usage| json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            })).unwrap_or_else(|| json!({ "input_tokens": 0, "output_tokens": 0 })),
        }))
    }
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

struct ToolBlock {
    index: u32,
    arguments: String,
}

/// Anthropic streaming state machine: `message_start` → `content_block_*` →
/// `message_delta` → `message_stop`, or an `error` event.
pub struct AnthropicStream {
    exposed_model: String,
    id: String,
    started: bool,
    next_index: u32,
    text_index: Option<u32>,
    tools: Vec<ToolBlock>,
    tool_by_call: HashMap<String, usize>,
    usage: Option<Usage>,
    terminal: bool,
}

impl AnthropicStream {
    fn new(exposed_model: String) -> Self {
        Self {
            exposed_model,
            id: "msg-polygate".into(),
            started: false,
            next_index: 0,
            text_index: None,
            tools: Vec::new(),
            tool_by_call: HashMap::new(),
            usage: None,
            terminal: false,
        }
    }

    fn emit(kind: &str, body: Value) -> Event {
        Event::default().event(kind).data(body.to_string())
    }

    fn start_message(&mut self) -> Vec<Event> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![Self::emit(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.exposed_model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    // Input tokens are only known when the upstream reports
                    // usage, which happens after the first content events.
                    // The final `message_delta` carries output tokens; the
                    // messages API has no later slot for input tokens.
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        )]
    }

    fn close_text(&mut self, frames: &mut Vec<Event>) {
        if let Some(index) = self.text_index.take() {
            frames.push(Self::emit(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
        }
    }

    fn ensure_tool(&mut self, call_id: &str, name: &str, frames: &mut Vec<Event>) -> usize {
        if let Some(position) = self.tool_by_call.get(call_id) {
            return *position;
        }
        let index = self.next_index;
        self.next_index += 1;
        frames.push(Self::emit(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": {},
                },
            }),
        ));
        self.tools.push(ToolBlock {
            index,
            arguments: String::new(),
        });
        let position = self.tools.len() - 1;
        self.tool_by_call.insert(call_id.to_string(), position);
        position
    }

    fn close_tools(&mut self, frames: &mut Vec<Event>) {
        for tool in std::mem::take(&mut self.tools) {
            frames.push(Self::emit(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": tool.index }),
            ));
        }
        self.tool_by_call.clear();
    }

    fn error_event(error: &GatewayError) -> Event {
        Self::emit("error", anthropic_error_body(error))
    }

    fn fail_terminal(&mut self, err: &GatewayError) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        self.terminal = true;
        vec![Self::error_event(err)]
    }
}

impl SseTranslator for AnthropicStream {
    fn handle(&mut self, event: StreamEvent) -> Vec<Event> {
        let mut frames = Vec::new();
        match event {
            StreamEvent::Start { id, .. } => {
                self.id = format!("msg-{id}");
                frames.extend(self.start_message());
            }
            StreamEvent::TextDelta { text } => {
                if !self.started {
                    frames.extend(self.start_message());
                }
                let index = match self.text_index {
                    Some(index) => index,
                    None => {
                        let index = self.next_index;
                        self.next_index += 1;
                        frames.push(Self::emit(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": { "type": "text", "text": "" },
                            }),
                        ));
                        self.text_index = Some(index);
                        index
                    }
                };
                frames.push(Self::emit(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "text_delta", "text": text },
                    }),
                ));
            }
            // Thinking is only rendered by Anthropic when the client asked for
            // it; mis-labelling reasoning as text would be dishonest.
            StreamEvent::ReasoningDelta { .. } => {}
            StreamEvent::ToolCallStart { id, name, .. } => {
                if !self.started {
                    frames.extend(self.start_message());
                }
                self.close_text(&mut frames);
                self.ensure_tool(&id, &name, &mut frames);
            }
            StreamEvent::ToolCallArgsDelta { id, delta } => {
                if !self.started {
                    frames.extend(self.start_message());
                }
                self.close_text(&mut frames);
                let position = self.ensure_tool(&id, "", &mut frames);
                let tool = &mut self.tools[position];
                tool.arguments.push_str(&delta);
                let index = tool.index;
                frames.push(Self::emit(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": delta },
                    }),
                ));
            }
            StreamEvent::Usage { usage } => self.usage = Some(usage),
            StreamEvent::Finish { reason } => {
                if !self.started {
                    frames.extend(self.start_message());
                }
                self.close_text(&mut frames);
                self.close_tools(&mut frames);
                let output_tokens = self.usage.map(|usage| usage.output_tokens).unwrap_or(0);
                frames.push(Self::emit(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {
                            "stop_reason": stop_reason(&reason),
                            "stop_sequence": Value::Null,
                        },
                        "usage": { "output_tokens": output_tokens },
                    }),
                ));
                frames.push(Self::emit(
                    "message_stop",
                    json!({ "type": "message_stop" }),
                ));
                self.terminal = true;
            }
            StreamEvent::Error { error } => {
                let error = GatewayError::new(error.kind, error.message);
                let event = Self::error_event(&error);
                self.terminal = true;
                frames.push(event);
            }
        }
        frames
    }

    fn end(&mut self) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        let error = GatewayError::upstream_server("upstream stream ended without a terminal event");
        self.fail_terminal(&error)
    }

    fn fail(&mut self, err: &GatewayError) -> Vec<Event> {
        self.fail_terminal(err)
    }
}
