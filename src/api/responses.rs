//! OpenAI Responses API (`POST /v1/responses`).
//!
//! The API is a second surface over the same internal protocol. Supported:
//! plain text input, multi-turn role messages, function tools with
//! `function_call` / `function_call_output` items, streaming events and usage.
//! Unsupported fields are rejected explicitly (`deny_unknown_fields`,
//! `previous_response_id`, non-function tools) instead of being silently
//! approximated.

use std::collections::HashMap;

use axum::response::sse::Event;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::GatewayError;
use crate::model_service::RouteNeeds;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, ToolDefinition, ToolResult, Usage,
};

use super::sse::SseTranslator;
use super::wire::{block_has_image, WireRequest};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub input: Option<ResponsesInput>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<ResponsesTool>>,
    #[serde(default)]
    pub tool_choice: Option<ResponsesToolChoice>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub store: Option<bool>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesItem>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsesItem {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub output: Option<Value>,
    /// Echoed from previous output; accepted and ignored.
    #[serde(default)]
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponsesToolChoice {
    Mode(String),
    Named {
        #[serde(rename = "type", default)]
        kind: Option<String>,
        name: String,
    },
}

impl ResponsesRequest {
    fn items(&self) -> &[ResponsesItem] {
        match &self.input {
            Some(ResponsesInput::Items(items)) => items,
            _ => &[],
        }
    }

    fn tool_definitions(&self) -> Result<Vec<ToolDefinition>, GatewayError> {
        let Some(tools) = &self.tools else {
            return Ok(Vec::new());
        };
        tools
            .iter()
            .map(|tool| {
                Ok(ToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: Some(tool.parameters.clone().unwrap_or_else(|| json!({}))),
                })
            })
            .collect()
    }

    fn tool_choice(&self) -> Result<Option<ToolChoice>, GatewayError> {
        Ok(match &self.tool_choice {
            None => None,
            Some(ResponsesToolChoice::Mode(mode)) => Some(match mode.as_str() {
                "auto" => ToolChoice::Auto,
                "none" => ToolChoice::None,
                "required" => ToolChoice::Required,
                other => {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported `tool_choice` value `{other}`"
                    )))
                }
            }),
            Some(ResponsesToolChoice::Named { kind, name }) => {
                if kind.as_deref().unwrap_or("function") != "function" {
                    return Err(GatewayError::bad_request(
                        "only function tool_choice is supported",
                    ));
                }
                if name.trim().is_empty() {
                    return Err(GatewayError::bad_request(
                        "tool_choice function name must not be empty",
                    ));
                }
                Some(ToolChoice::Tool(name.clone()))
            }
        })
    }
}

fn message_parts(content: &Value, role: MessageRole) -> Result<Vec<ContentBlock>, GatewayError> {
    match content {
        Value::String(text) => Ok(vec![ContentBlock::text(text.clone())]),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::bad_request("content part requires `type`"))?;
                match kind {
                    "input_text" | "output_text" | "text" => {
                        let text = part.get("text").and_then(Value::as_str).ok_or_else(|| {
                            GatewayError::bad_request("text part requires `text`")
                        })?;
                        blocks.push(ContentBlock::text(text));
                    }
                    "input_image" | "image_url" | "image" => {
                        blocks.push(ContentBlock::Unsupported {
                            media_type: "image".into(),
                            data: part.clone(),
                        });
                    }
                    other => {
                        return Err(GatewayError::bad_request(format!(
                            "unsupported content part `{other}` for `{}` input",
                            role.as_str()
                        )))
                    }
                }
            }
            Ok(blocks)
        }
        Value::Null => Ok(Vec::new()),
        _ => Err(GatewayError::bad_request(
            "content must be a string or an array of parts",
        )),
    }
}

fn parse_role(role: &str) -> Result<MessageRole, GatewayError> {
    match role {
        "system" => Ok(MessageRole::System),
        "developer" => Ok(MessageRole::Developer),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        other => Err(GatewayError::bad_request(format!(
            "unsupported input role `{other}`"
        ))),
    }
}

fn output_text(value: &Value) -> Result<String, GatewayError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("input_text");
                if kind != "input_text" && kind != "output_text" && kind != "text" {
                    return Err(GatewayError::bad_request(format!(
                        "unsupported function_call_output part `{kind}`"
                    )));
                }
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| GatewayError::bad_request("text part requires `text`"))?;
                out.push_str(text);
            }
            Ok(out)
        }
        _ => Err(GatewayError::bad_request(
            "function_call_output `output` must be a string or text parts",
        )),
    }
}

impl WireRequest for ResponsesRequest {
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
        if self.previous_response_id.is_some() {
            return Err(GatewayError::bad_request(
                "`previous_response_id` is not supported; send the full conversation in `input`",
            ));
        }
        match &self.input {
            None => return Err(GatewayError::bad_request("`input` is required")),
            Some(ResponsesInput::Text(text)) if text.trim().is_empty() => {
                return Err(GatewayError::bad_request("`input` must not be empty"))
            }
            Some(ResponsesInput::Items(items)) if items.is_empty() => {
                return Err(GatewayError::bad_request("`input` must not be empty"))
            }
            _ => {}
        }
        if self.store == Some(true) {
            return Err(GatewayError::bad_request(
                "response storage is not supported; pass `store: false`",
            ));
        }
        if self.parallel_tool_calls == Some(false) {
            return Err(GatewayError::bad_request(
                "`parallel_tool_calls: false` is not supported",
            ));
        }
        if self.max_output_tokens == Some(0) {
            return Err(GatewayError::bad_request(
                "`max_output_tokens` must be greater than zero",
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
                if tool.name.trim().is_empty() {
                    return Err(GatewayError::bad_request("tool name is required"));
                }
                if tool.strict == Some(true) {
                    return Err(GatewayError::bad_request(
                        "`strict` tool schemas are not supported",
                    ));
                }
            }
        }
        if matches!(self.tool_choice, Some(ResponsesToolChoice::Named { .. }))
            && self.tools.as_ref().map(Vec::is_empty).unwrap_or(true)
        {
            return Err(GatewayError::bad_request(
                "`tool_choice` names a function but `tools` is empty",
            ));
        }
        self.tool_choice()?;
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
        for item in self.items() {
            if let Some(Value::Array(parts)) = &item.content {
                vision |= parts.iter().any(block_has_image);
            }
        }
        RouteNeeds {
            streaming: self.streaming(),
            tools,
            vision,
        }
    }

    fn api_name(&self) -> &'static str {
        "openai.responses"
    }

    fn first_user_text(&self) -> Option<String> {
        match &self.input {
            Some(ResponsesInput::Text(text)) if !text.trim().is_empty() => Some(text.clone()),
            Some(ResponsesInput::Items(items)) => {
                for item in items {
                    if item.role.as_deref() != Some("user") {
                        continue;
                    }
                    let content = item.content.as_ref()?;
                    let text = match content {
                        Value::String(text) => text.clone(),
                        Value::Array(parts) => parts
                            .iter()
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
            _ => None,
        }
    }

    fn to_chat_request(
        &self,
        upstream_model: &str,
        request_id: String,
    ) -> Result<ChatRequest, GatewayError> {
        let mut messages = Vec::new();
        if let Some(instructions) = &self.instructions {
            if !instructions.trim().is_empty() {
                messages.push(ChatMessage::new(
                    MessageRole::System,
                    vec![ContentBlock::text(instructions.clone())],
                ));
            }
        }

        match &self.input {
            Some(ResponsesInput::Text(text)) => {
                messages.push(ChatMessage::new(
                    MessageRole::User,
                    vec![ContentBlock::text(text.clone())],
                ));
            }
            Some(ResponsesInput::Items(items)) => {
                for item in items {
                    match item.kind.as_deref().unwrap_or("message") {
                        "message" => {
                            let role = parse_role(item.role.as_deref().ok_or_else(|| {
                                GatewayError::bad_request("message item requires `role`")
                            })?)?;
                            let content = item.content.as_ref().ok_or_else(|| {
                                GatewayError::bad_request("message item requires `content`")
                            })?;
                            messages.push(ChatMessage::new(role, message_parts(content, role)?));
                        }
                        "function_call" => {
                            let call_id = item.call_id.as_ref().ok_or_else(|| {
                                GatewayError::bad_request("function_call requires `call_id`")
                            })?;
                            let name = item.name.as_ref().ok_or_else(|| {
                                GatewayError::bad_request("function_call requires `name`")
                            })?;
                            let call = ToolCall::new(
                                call_id.clone(),
                                name.clone(),
                                item.arguments.clone().unwrap_or_default(),
                            );
                            // Consecutive calls belong to one assistant turn.
                            match messages.last_mut() {
                                Some(message)
                                    if message.role == MessageRole::Assistant
                                        && message.content.iter().all(|block| {
                                            matches!(block, ContentBlock::ToolCall(_))
                                        }) =>
                                {
                                    message.content.push(ContentBlock::ToolCall(call));
                                }
                                _ => messages.push(ChatMessage::new(
                                    MessageRole::Assistant,
                                    vec![ContentBlock::ToolCall(call)],
                                )),
                            }
                        }
                        "function_call_output" => {
                            let call_id = item.call_id.as_ref().ok_or_else(|| {
                                GatewayError::bad_request("function_call_output requires `call_id`")
                            })?;
                            let output = item.output.as_ref().ok_or_else(|| {
                                GatewayError::bad_request("function_call_output requires `output`")
                            })?;
                            messages.push(ChatMessage::tool(ToolResult::new(
                                call_id.clone(),
                                output_text(output)?,
                            )));
                        }
                        other => {
                            return Err(GatewayError::bad_request(format!(
                                "unsupported input item type `{other}`"
                            )))
                        }
                    }
                }
            }
            None => {}
        }

        if messages.is_empty() {
            return Err(GatewayError::bad_request("`input` produced no messages"));
        }

        Ok(ChatRequest {
            request_id: Some(request_id),
            model: upstream_model.to_string(),
            messages,
            tools: self.tool_definitions()?,
            tool_choice: self.tool_choice()?,
            max_output_tokens: self.max_output_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            stop: Vec::new(),
            stream: self.streaming(),
            metadata: self
                .metadata
                .clone()
                .or_else(|| self.user.as_ref().map(|user| json!({ "user": user }))),
        })
    }

    fn translator(&self, exposed_model: String) -> Box<dyn SseTranslator> {
        Box::new(ResponsesStream::new(exposed_model))
    }

    fn render(&self, response: &ChatResponse, exposed_model: &str) -> Result<Value, GatewayError> {
        let output = output_items(response);
        let status = response_status(&response.finish_reason);
        let mut body = response_base(
            &format!("resp-{}", response.id),
            exposed_model,
            response.created_at.timestamp(),
            status,
            output,
            response.usage,
        );
        body["output_text"] = json!(response.text());
        if status == "incomplete" {
            body["incomplete_details"] = json!({
                "reason": match response.finish_reason {
                    FinishReason::Length => "max_output_tokens",
                    FinishReason::ContentFilter => "content_filter",
                    _ => "unknown",
                }
            });
        }
        Ok(body)
    }
}

fn response_status(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Length | FinishReason::ContentFilter => "incomplete",
        _ => "completed",
    }
}

fn response_base(
    id: &str,
    model: &str,
    created: i64,
    status: &str,
    output: Vec<Value>,
    usage: Option<Usage>,
) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": created,
        "status": status,
        "model": model,
        "output": output,
        "usage": usage.map(|usage| json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total(),
        })),
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "metadata": Value::Null,
    })
}

fn output_items(response: &ChatResponse) -> Vec<Value> {
    let mut output = Vec::new();
    let text = response.text();
    let calls: Vec<&ToolCall> = response.message.tool_calls().collect();
    if !text.is_empty() || calls.is_empty() {
        output.push(json!({
            "id": format!("msg-{}", response.id),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
            }],
        }));
    }
    for (index, call) in calls.iter().enumerate() {
        output.push(json!({
            "id": format!("fc-{}-{index}", response.id),
            "type": "function_call",
            "status": "completed",
            "call_id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }));
    }
    output
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

struct TextState {
    index: u32,
    item_id: String,
    text: String,
}

struct ToolState {
    index: u32,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    opened: bool,
}

/// Responses streaming state machine: `response.created` → item/content-part
/// lifecycle → `response.completed` (or `response.failed`).
pub struct ResponsesStream {
    exposed_model: String,
    response_id: String,
    created: i64,
    sequence: u64,
    next_index: u32,
    output: Vec<Value>,
    text: Option<TextState>,
    tools: Vec<ToolState>,
    tool_by_call: HashMap<String, usize>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    terminal: bool,
}

impl ResponsesStream {
    fn new(exposed_model: String) -> Self {
        Self {
            exposed_model,
            response_id: "resp-polygate".into(),
            created: Utc::now().timestamp(),
            sequence: 0,
            next_index: 0,
            output: Vec::new(),
            text: None,
            tools: Vec::new(),
            tool_by_call: HashMap::new(),
            usage: None,
            finish: None,
            terminal: false,
        }
    }

    fn emit(&mut self, kind: &str, body: Value) -> Event {
        self.sequence += 1;
        let mut body = body;
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        Event::default().event(kind).data(body.to_string())
    }

    fn response_object(&self, status: &str, output: Vec<Value>) -> Value {
        response_base(
            &self.response_id,
            &self.exposed_model,
            self.created,
            status,
            output,
            self.usage,
        )
    }

    fn base_id(&self) -> &str {
        self.response_id
            .strip_prefix("resp-")
            .unwrap_or(&self.response_id)
    }

    fn take_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn ensure_text(&mut self, frames: &mut Vec<Event>) {
        if self.text.is_some() {
            return;
        }
        let index = self.take_index();
        let item_id = format!("msg-{}", self.base_id());
        frames.push(self.emit(
            "response.output_item.added",
            json!({
                "output_index": index,
                "item": {
                    "id": item_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        ));
        frames.push(self.emit(
            "response.content_part.added",
            json!({
                "item_id": item_id,
                "output_index": index,
                "content_index": 0,
                "part": { "type": "output_text", "text": "", "annotations": [] },
            }),
        ));
        self.text = Some(TextState {
            index,
            item_id,
            text: String::new(),
        });
    }

    fn close_text(&mut self, frames: &mut Vec<Event>) {
        let Some(state) = self.text.take() else {
            return;
        };
        frames.push(self.emit(
            "response.output_text.done",
            json!({
                "item_id": state.item_id,
                "output_index": state.index,
                "content_index": 0,
                "text": state.text.clone(),
            }),
        ));
        frames.push(self.emit(
            "response.content_part.done",
            json!({
                "item_id": state.item_id,
                "output_index": state.index,
                "content_index": 0,
                "part": { "type": "output_text", "text": state.text.clone(), "annotations": [] },
            }),
        ));
        let item = json!({
            "id": state.item_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": state.text.clone(), "annotations": [] }],
        });
        frames.push(self.emit(
            "response.output_item.done",
            json!({ "output_index": state.index, "item": item.clone() }),
        ));
        self.output.push(item);
    }

    fn ensure_tool(&mut self, call_id: &str, name: &str, frames: &mut Vec<Event>) -> usize {
        if let Some(position) = self.tool_by_call.get(call_id) {
            return *position;
        }
        let index = self.take_index();
        let item_id = format!("fc-{}-{}", self.base_id(), index);
        frames.push(self.emit(
            "response.output_item.added",
            json!({
                "output_index": index,
                "item": {
                    "id": item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": name,
                    "arguments": "",
                },
            }),
        ));
        self.tools.push(ToolState {
            index,
            item_id,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: String::new(),
            opened: true,
        });
        let position = self.tools.len() - 1;
        self.tool_by_call.insert(call_id.to_string(), position);
        position
    }

    fn close_tools(&mut self, frames: &mut Vec<Event>) {
        let tools = std::mem::take(&mut self.tools);
        for tool in tools {
            if !tool.opened {
                continue;
            }
            frames.push(self.emit(
                "response.function_call_arguments.done",
                json!({
                    "item_id": tool.item_id,
                    "output_index": tool.index,
                    "arguments": tool.arguments,
                }),
            ));
            let item = json!({
                "id": tool.item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": tool.call_id,
                "name": tool.name,
                "arguments": tool.arguments,
            });
            frames.push(self.emit(
                "response.output_item.done",
                json!({ "output_index": tool.index, "item": item.clone() }),
            ));
            self.output.push(item);
        }
        self.tool_by_call.clear();
    }

    fn failed(&mut self, error: &GatewayError) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        self.terminal = true;
        let mut response = self.response_object("failed", self.output.clone());
        response["error"] = json!({
            "code": error.kind.code(),
            "message": error.message,
        });
        vec![self.emit("response.failed", json!({ "response": response }))]
    }
}

impl SseTranslator for ResponsesStream {
    fn handle(&mut self, event: StreamEvent) -> Vec<Event> {
        let mut frames = Vec::new();
        match event {
            StreamEvent::Start { id, .. } => {
                self.response_id = format!("resp-{id}");
                let response = self.response_object("in_progress", Vec::new());
                frames.push(self.emit("response.created", json!({ "response": response })));
            }
            StreamEvent::TextDelta { text } => {
                self.ensure_text(&mut frames);
                if let Some(state) = self.text.as_mut() {
                    state.text.push_str(&text);
                    let (item_id, index) = (state.item_id.clone(), state.index);
                    frames.push(self.emit(
                        "response.output_text.delta",
                        json!({
                            "item_id": item_id,
                            "output_index": index,
                            "content_index": 0,
                            "delta": text,
                        }),
                    ));
                }
            }
            // The Responses API represents reasoning as a separate item type
            // with encrypted content; the gateway has no equivalent, so
            // reasoning deltas are not re-labelled as visible text.
            StreamEvent::ReasoningDelta { .. } => {}
            StreamEvent::ToolCallStart { id, name, .. } => {
                self.close_text(&mut frames);
                self.ensure_tool(&id, &name, &mut frames);
            }
            StreamEvent::ToolCallArgsDelta { id, delta } => {
                self.close_text(&mut frames);
                let position = self.ensure_tool(&id, "", &mut frames);
                let tool = &mut self.tools[position];
                tool.arguments.push_str(&delta);
                let (item_id, index) = (tool.item_id.clone(), tool.index);
                frames.push(self.emit(
                    "response.function_call_arguments.delta",
                    json!({
                        "item_id": item_id,
                        "output_index": index,
                        "delta": delta,
                    }),
                ));
            }
            StreamEvent::Usage { usage } => self.usage = Some(usage),
            StreamEvent::Finish { reason } => {
                self.finish = Some(reason.clone());
                self.close_text(&mut frames);
                self.close_tools(&mut frames);
                let status = response_status(&reason);
                let mut response = self.response_object(status, self.output.clone());
                if status == "incomplete" {
                    response["incomplete_details"] = json!({
                        "reason": match reason {
                            FinishReason::Length => "max_output_tokens",
                            FinishReason::ContentFilter => "content_filter",
                            _ => "unknown",
                        }
                    });
                }
                frames.push(self.emit("response.completed", json!({ "response": response })));
                self.terminal = true;
            }
            StreamEvent::Error { error } => {
                let error = GatewayError::new(error.kind, error.message);
                frames.extend(self.failed(&error));
            }
        }
        frames
    }

    fn end(&mut self) -> Vec<Event> {
        if self.terminal {
            return Vec::new();
        }
        let error = GatewayError::upstream_server("upstream stream ended without a terminal event");
        self.failed(&error)
    }

    fn fail(&mut self, err: &GatewayError) -> Vec<Event> {
        self.failed(err)
    }
}
