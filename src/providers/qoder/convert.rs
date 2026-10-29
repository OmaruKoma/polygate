//! Internal protocol <-> Qoder `agent_chat_generation` conversion.
//!
//! Source: Coding2API `src/provider/qoder/client.py` (`build_chat_body`,
//! `_normalize_null_content`, `_drop_orphan_tool_messages`) at commit
//! `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! What the upstream requires (source evidenced):
//! * `stream` is always `true`; there is no non-streaming upstream mode.
//! * `agent_id` is the fixed `agent_common`.
//! * assistant `content` must not be `null`: an assistant with tool calls and no
//!   text uses `""`, otherwise the following `role:"tool"` message becomes an
//!   orphan and the upstream rejects the tool pairing.
//! * `role:"tool"` messages must follow an assistant that declared their
//!   `tool_call_id`; consecutive tool results share the preceding assistant's
//!   declaration set, and consecutive assistants union their declarations.
//!
//! What this module cannot represent: `ToolResult::is_error` has no OpenAI
//! `role:"tool"` field. The content is forwarded unchanged; no synthetic flag is
//! invented because an unknown field may be rejected by the upstream.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, Usage,
};

pub const AGENT_ID: &str = "agent_common";

/// Serialize the body exactly as it is sent and apply the COSY Base64 variant.
/// Signing must use this string, not a re-serialized copy.
pub fn encode_body(body: &Value) -> Result<String, GatewayError> {
    let raw = serde_json::to_vec(body)
        .map_err(|_| GatewayError::internal("cannot serialize the Qoder request body"))?;
    Ok(super::cosy::qoder_encode(&raw))
}

/// Build the JSON body that is later `qoder_encode`d and signed.
pub fn build_body(request: &ChatRequest) -> Result<Value, GatewayError> {
    // Key order follows the reference builder; the signature covers the exact
    // encoded bytes, so a stable order also keeps fixtures reproducible.
    let mut body = Map::new();
    body.insert(
        "messages".into(),
        Value::Array(messages_to_upstream(&request.messages)?),
    );
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert("agent_id".into(), json!(AGENT_ID));

    if !request.tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools_to_upstream(request)));
    }
    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_upstream(choice));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if !request.stop.is_empty() {
        body.insert("stop".into(), json!(request.stop));
    }

    let mut parameters = Map::new();
    if let Some(max_tokens) = request.max_output_tokens {
        parameters.insert("max_tokens".into(), json!(max_tokens));
    }
    if !parameters.is_empty() {
        body.insert("parameters".into(), Value::Object(parameters));
    }

    Ok(Value::Object(body))
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        out.extend(message_to_upstream(message)?);
    }
    let (kept, _dropped) = drop_orphan_tool_messages(out);
    Ok(kept)
}

fn message_to_upstream(message: &ChatMessage) -> Result<Vec<Value>, GatewayError> {
    for block in &message.content {
        if let ContentBlock::Unsupported { media_type, .. } = block {
            return Err(GatewayError::new(
                ErrorKind::Unsupported,
                format!("Qoder adapter cannot forward `{media_type}` content blocks"),
            ));
        }
    }

    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let reasoning: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let tool_calls: Vec<&ToolCall> = message.tool_calls().collect();
    let tool_results: Vec<&crate::protocol::ToolResult> = message.tool_results().collect();

    match message.role {
        MessageRole::Tool => {
            if tool_results.is_empty() {
                return Err(GatewayError::bad_request(
                    "a `tool` message must carry a tool result",
                ));
            }
            if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                return Err(GatewayError::bad_request(
                    "a `tool` message cannot also carry text, reasoning or tool calls",
                ));
            }
            if message.name.is_some() {
                return Err(GatewayError::bad_request(
                    "a `tool` message cannot carry a participant name",
                ));
            }
            Ok(tool_results
                .into_iter()
                .map(|result| {
                    json!({
                        "role": "tool",
                        "tool_call_id": result.tool_call_id,
                        "content": result.content,
                    })
                })
                .collect())
        }
        MessageRole::Assistant => {
            if !tool_results.is_empty() {
                return Err(GatewayError::bad_request(
                    "an `assistant` message cannot carry a tool result",
                ));
            }
            let mut object = Map::new();
            object.insert("role".into(), json!("assistant"));
            if let Some(name) = &message.name {
                object.insert("name".into(), json!(name));
            }
            // Never emit `null`: Qoder treats it as an empty message and then
            // rejects the following tool result as an orphan.
            object.insert("content".into(), json!(text));
            if !reasoning.is_empty() {
                object.insert("reasoning_content".into(), json!(reasoning));
            }
            if !tool_calls.is_empty() {
                object.insert(
                    "tool_calls".into(),
                    Value::Array(tool_calls.into_iter().map(tool_call_to_upstream).collect()),
                );
            }
            Ok(vec![Value::Object(object)])
        }
        role => {
            if !tool_calls.is_empty() || !tool_results.is_empty() {
                return Err(GatewayError::bad_request(
                    "only an assistant message may declare tool calls",
                ));
            }
            if !reasoning.is_empty() {
                return Err(GatewayError::bad_request(
                    "reasoning content is only valid on assistant messages",
                ));
            }
            let mut object = Map::new();
            // The upstream accepts OpenAI messages; `developer` is normalised to
            // `system` because there is no evidence it understands `developer`.
            object.insert("role".into(), json!(role_name(role)));
            if let Some(name) = &message.name {
                object.insert("name".into(), json!(name));
            }
            object.insert("content".into(), json!(text));
            Ok(vec![Value::Object(object)])
        }
    }
}

fn tool_call_to_upstream(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.arguments,
        },
    })
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System | MessageRole::Developer => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn tools_to_upstream(request: &ChatRequest) -> Vec<Value> {
    request
        .tools
        .iter()
        .map(|tool| {
            let mut function = Map::new();
            function.insert("name".into(), json!(tool.name));
            if let Some(description) = &tool.description {
                function.insert("description".into(), json!(description));
            }
            function.insert(
                "parameters".into(),
                tool.parameters
                    .clone()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            json!({"type": "function", "function": Value::Object(function)})
        })
        .collect()
}

fn tool_choice_to_upstream(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

/// Drop `role:"tool"` messages that do not follow an assistant declaring their
/// `tool_call_id`. The upstream rejects such histories with a tool-pairing 400,
/// even when the orphan was produced by a client-side history edit.
fn drop_orphan_tool_messages(messages: Vec<Value>) -> (Vec<Value>, usize) {
    let mut kept = Vec::with_capacity(messages.len());
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut previous: Option<String> = None;
    let mut dropped = 0usize;

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if role == "tool" {
            let call_id = message.get("tool_call_id").and_then(Value::as_str);
            let known = call_id.is_some_and(|id| declared.contains(id));
            if !known {
                dropped += 1;
                continue;
            }
            kept.push(message);
            previous = Some(role);
            continue;
        }

        if role == "assistant" {
            let ids: BTreeSet<String> = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| call.get("id").and_then(Value::as_str))
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            declared = if previous.as_deref() == Some("assistant") && !ids.is_empty() {
                declared.union(&ids).cloned().collect()
            } else {
                ids
            };
        } else {
            declared.clear();
        }

        previous = Some(role);
        kept.push(message);
    }

    (kept, dropped)
}

/// Aggregate translated stream events into a non-streamed response. An
/// `Error` event is returned as an error, never as a successful response.
pub fn aggregate(
    response_id: &str,
    request_id: Option<String>,
    model: &str,
    events: &[StreamEvent],
) -> Result<ChatResponse, GatewayError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut tool_positions: HashMap<String, usize> = HashMap::new();
    let mut usage: Option<Usage> = None;
    let mut finish: Option<FinishReason> = None;

    for event in events {
        match event {
            StreamEvent::TextDelta { text: part } => text.push_str(part),
            StreamEvent::ReasoningDelta { text: part } => reasoning.push_str(part),
            StreamEvent::ToolCallStart { id, name, .. } => {
                if let Some(position) = tool_positions.get(id) {
                    if tool_calls[*position].name.is_empty() {
                        tool_calls[*position].name = name.clone();
                    }
                } else {
                    tool_positions.insert(id.clone(), tool_calls.len());
                    tool_calls.push(ToolCall::new(id.clone(), name.clone(), ""));
                }
            }
            StreamEvent::ToolCallArgsDelta { id, delta } => {
                let position = *tool_positions.entry(id.clone()).or_insert_with(|| {
                    tool_calls.push(ToolCall::new(id.clone(), "", ""));
                    tool_calls.len() - 1
                });
                tool_calls[position].arguments.push_str(delta);
            }
            StreamEvent::Usage { usage: value } => usage = Some(*value),
            StreamEvent::Finish { reason } => finish = Some(reason.clone()),
            StreamEvent::Error { error } => {
                return Err(GatewayError::new(error.kind, error.message.clone()));
            }
            StreamEvent::Start { .. } => {}
        }
    }

    let mut message = ChatMessage::new(MessageRole::Assistant, Vec::new());
    if !reasoning.is_empty() {
        message.content.push(ContentBlock::reasoning(reasoning));
    }
    if !text.is_empty() {
        message.content.push(ContentBlock::text(text));
    }
    for call in tool_calls {
        message.content.push(ContentBlock::ToolCall(call));
    }

    let mut response = ChatResponse::new(
        response_id.to_string(),
        model.to_string(),
        message,
        finish.unwrap_or(FinishReason::Unknown),
    );
    response.request_id = request_id;
    response.usage = usage;
    Ok(response)
}
