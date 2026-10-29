//! Internal protocol → TRAE `llm_utils_chat` body conversion.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §5, built from
//! `Coding2API/src/provider/trae/client.py:92-255` (`prepare_body`,
//! `_normalize_tools`, `_stringify_tool_parameters`).
//!
//! The endpoint is not OpenAI-compatible: it expects its own envelope
//! (`function`, `config_name`, `model`, `stream`) and its own message shape. The
//! transformations below are the ones the reference implementation needed in
//! practice:
//!
//! * `developer` → `system` (TRAE silently returns an empty stream otherwise);
//! * message content string → `[{"type":"text","text":…}]`;
//! * assistant `tool_calls[].function` → `function_call`;
//! * `tools[].function.parameters` object → JSON **string** (TRAE answers
//!   `4001 cannot unmarshal object … into type string` otherwise);
//! * `tool_choice: "none"` drops `tools`; a forced function becomes its name.
//!
//! Deliberate difference from the reference: an orphan `tool` result or an
//! assistant tool call without a name is a `BadRequest` here. The reference
//! silently drops both to keep a dirty history usable; the gateway's history
//! integrity rule says a corrupted tool pairing must be visible instead of
//! being rewritten behind the caller's back.
//!
//! OpenAI sampling fields (`temperature`, `top_p`, `max_tokens`, `stop`) are not
//! forwarded: no source evidences them in this envelope, and inventing them
//! would be a protocol guess.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::error::GatewayError;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, Usage,
};

use super::region::DEFAULT_FUNCTION;

pub fn build_body(request: &ChatRequest) -> Result<Value, GatewayError> {
    let model = request.model.trim();
    if model.is_empty() {
        return Err(GatewayError::bad_request("TRAE request has no model"));
    }
    let messages = messages_to_upstream(&request.messages)?;
    let tools = tools_to_upstream(&request.tools)?;

    let mut body = Map::new();
    body.insert("function".into(), json!(DEFAULT_FUNCTION));
    body.insert("config_name".into(), json!(model));
    body.insert("model".into(), json!(model));
    body.insert("stream".into(), json!(true));
    body.insert("messages".into(), Value::Array(messages));
    match &request.tool_choice {
        // TRAE rejects a request that carries a tool policy with no tools.
        Some(ToolChoice::None) => {}
        choice => {
            if !tools.is_empty() {
                body.insert("tools".into(), Value::Array(tools));
                if let Some(choice) = choice {
                    body.insert("tool_choice".into(), tool_choice_to_upstream(choice));
                }
            }
        }
    }
    Ok(Value::Object(body))
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    // Every assistant tool call must be declared before a `tool` result may
    // reference it. The reference collects ids over the whole history, so a
    // result that precedes its assistant call is still accepted.
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for call in message.tool_calls() {
            validate_call(call)?;
            declared.insert(call.id.clone());
        }
    }

    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        out.extend(message_to_upstream(message, &declared)?);
    }
    Ok(out)
}

fn validate_call(call: &ToolCall) -> Result<(), GatewayError> {
    if call.name.trim().is_empty() {
        return Err(GatewayError::bad_request(
            "TRAE assistant tool call has no function name",
        ));
    }
    if call.id.trim().is_empty() {
        return Err(GatewayError::bad_request(
            "TRAE assistant tool call has no id; tool results cannot be paired",
        ));
    }
    Ok(())
}

fn message_to_upstream(
    message: &ChatMessage,
    declared: &BTreeSet<String>,
) -> Result<Vec<Value>, GatewayError> {
    for block in &message.content {
        if let ContentBlock::Unsupported { media_type, .. } = block {
            return Err(GatewayError::bad_request(format!(
                "TRAE adapter cannot forward `{media_type}` content blocks"
            )));
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
    let tool_calls: Vec<&ToolCall> = message.tool_calls().collect();
    let tool_results: Vec<&crate::protocol::ToolResult> = message.tool_results().collect();

    match message.role {
        MessageRole::Tool => {
            if tool_results.is_empty() {
                return Err(GatewayError::bad_request(
                    "a `tool` message must carry a tool result",
                ));
            }
            if !text.is_empty() || !tool_calls.is_empty() {
                return Err(GatewayError::bad_request(
                    "a `tool` message cannot also carry text or tool calls",
                ));
            }
            let mut out = Vec::with_capacity(tool_results.len());
            for result in tool_results {
                if !declared.contains(&result.tool_call_id) {
                    return Err(GatewayError::bad_request(format!(
                        "tool result `{}` does not belong to an assistant tool call",
                        result.tool_call_id
                    )));
                }
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": result.tool_call_id,
                    "content": [{"type": "text", "text": result.content}],
                }));
            }
            Ok(out)
        }
        MessageRole::Assistant => {
            if !tool_results.is_empty() {
                return Err(GatewayError::bad_request(
                    "an `assistant` message cannot carry a tool result",
                ));
            }
            for call in &tool_calls {
                validate_call(call)?;
            }
            let mut object = Map::new();
            object.insert("role".into(), json!("assistant"));
            object.insert(
                "content".into(),
                match (text.is_empty(), tool_calls.is_empty()) {
                    // The IDE sends a null placeholder for a pure tool-call turn.
                    (true, false) => Value::Null,
                    _ => Value::Array(vec![json!({"type": "text", "text": text})]),
                },
            );
            if !tool_calls.is_empty() {
                object.insert(
                    "tool_calls".into(),
                    Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| tool_call_to_upstream(call))
                            .collect(),
                    ),
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
            Ok(vec![json!({
                "role": role_name(role),
                "content": [{"type": "text", "text": text}],
            })])
        }
    }
}

fn tool_call_to_upstream(call: &ToolCall) -> Value {
    json!({
        "id": call.id,
        "type": "function",
        "function_call": {
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

fn tools_to_upstream(
    tools: &[crate::protocol::ToolDefinition],
) -> Result<Vec<Value>, GatewayError> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool.name.trim().is_empty() {
            return Err(GatewayError::bad_request(
                "TRAE tool definition has no function name",
            ));
        }
        let mut function = Map::new();
        function.insert("name".into(), json!(tool.name));
        if let Some(description) = &tool.description {
            function.insert("description".into(), json!(description));
        }
        // The upstream declares this field as a string, not a schema object.
        function.insert(
            "parameters".into(),
            json!(match &tool.parameters {
                Some(parameters) => serde_json::to_string(parameters).map_err(|_| {
                    GatewayError::bad_request("TRAE tool parameters cannot be serialized")
                })?,
                None => "{}".to_string(),
            }),
        );
        out.push(json!({"type": "function", "function": Value::Object(function)}));
    }
    Ok(out)
}

fn tool_choice_to_upstream(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!(name),
    }
}

/// Aggregate translated events into one response. An `Error` event becomes an
/// error, never a successful response.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ToolDefinition, ToolResult};

    fn assistant_with_call(id: &str) -> ChatMessage {
        let mut message = ChatMessage::new(MessageRole::Assistant, Vec::new());
        message
            .content
            .push(ContentBlock::ToolCall(ToolCall::new(id, "lookup", "{}")));
        message
    }

    #[test]
    fn envelope_and_content_are_rewritten() {
        let request = ChatRequest::new(
            "glm-5.2",
            vec![ChatMessage::new(
                MessageRole::Developer,
                vec![ContentBlock::text("rules")],
            )],
        );
        let body = build_body(&request).unwrap();
        assert_eq!(body["function"], DEFAULT_FUNCTION);
        assert_eq!(body["config_name"], "glm-5.2");
        assert_eq!(body["model"], "glm-5.2");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            json!([{"type": "text", "text": "rules"}])
        );
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn assistant_tool_calls_use_function_call() {
        let request = ChatRequest::new(
            "m",
            vec![
                assistant_with_call("c1"),
                ChatMessage::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult(ToolResult::new("c1", "ok"))],
                ),
            ],
        );
        let body = build_body(&request).unwrap();
        let message = &body["messages"][0];
        assert_eq!(message["content"], Value::Null);
        assert_eq!(message["tool_calls"][0]["function_call"]["name"], "lookup");
        assert!(message["tool_calls"][0].get("function").is_none());
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "c1");
    }

    #[test]
    fn an_orphan_tool_result_is_a_bad_request() {
        let request = ChatRequest::new(
            "m",
            vec![ChatMessage::new(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult(ToolResult::new("c1", "ok"))],
            )],
        );
        let error = build_body(&request).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::BadRequest);
    }

    #[test]
    fn an_unnamed_tool_call_is_a_bad_request() {
        let mut message = ChatMessage::new(MessageRole::Assistant, Vec::new());
        message
            .content
            .push(ContentBlock::ToolCall(ToolCall::new("c1", "", "{}")));
        let error = build_body(&ChatRequest::new("m", vec![message])).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::BadRequest);
    }

    #[test]
    fn tool_parameters_are_stringified() {
        let mut request = ChatRequest::new("m", vec![]);
        request.tools.push(ToolDefinition {
            name: "lookup".into(),
            description: Some("find".into()),
            parameters: Some(json!({"type": "object", "properties": {}})),
        });
        request.tools.push(ToolDefinition {
            name: "noparams".into(),
            description: None,
            parameters: None,
        });
        let body = build_body(&request).unwrap();
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            "{\"type\":\"object\",\"properties\":{}}"
        );
        assert_eq!(body["tools"][1]["function"]["parameters"], "{}");
    }

    #[test]
    fn tool_choice_none_drops_the_tools() {
        let mut request = ChatRequest::new("m", vec![]);
        request
            .tools
            .push(ToolDefinition::new("lookup", json!({"type": "object"})));
        request.tool_choice = Some(ToolChoice::None);
        let body = build_body(&request).unwrap();
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn a_forced_function_becomes_its_name() {
        let mut request = ChatRequest::new("m", vec![]);
        request
            .tools
            .push(ToolDefinition::new("lookup", json!({"type": "object"})));
        request.tool_choice = Some(ToolChoice::Tool("lookup".into()));
        let body = build_body(&request).unwrap();
        assert_eq!(body["tool_choice"], "lookup");
    }

    #[test]
    fn a_tool_policy_without_tools_is_omitted() {
        let mut request = ChatRequest::new("m", vec![]);
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_body(&request).unwrap();
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn aggregate_keeps_text_reasoning_tools_and_usage() {
        let events = vec![
            StreamEvent::start("r", None, "m"),
            StreamEvent::text_delta("Hi"),
            StreamEvent::reasoning_delta("why"),
            StreamEvent::ToolCallStart {
                id: "c1".into(),
                name: "lookup".into(),
                index: Some(0),
            },
            StreamEvent::tool_call_args_delta("c1", "{}"),
            StreamEvent::usage(Usage::new(10, 2)),
            StreamEvent::finish(FinishReason::ToolCalls),
        ];
        let response = aggregate("r", None, "m", &events).unwrap();
        assert_eq!(response.usage.map(|usage| usage.total()), Some(12));
        assert!(matches!(response.finish_reason, FinishReason::ToolCalls));
        assert_eq!(response.message.content.len(), 3);
    }
}
