//! Internal protocol <-> WorkBuddy `/v2/chat/completions` conversion.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §8-§9. The upstream accepts an
//! OpenAI-shaped body and always streams; `stream` is therefore forced to
//! `true` and the adapter aggregates when the caller asked for one response.
//!
//! Thinking control: the upstream only reads the flat `reasoning_effort` and
//! `thinking: {"type":"disabled"}`. When neither is sent the model applies its
//! own default and some models write the chain of thought into the visible
//! `content` instead of `reasoning_content` (`WORKBUDDY_MIGRATION.md` §5;
//! reference `workbuddy2api/upstream_compat.py` `apply_thinking_intent`). The
//! caller's intent is therefore translated, never invented:
//! * an effort (or "off") from the caller is forwarded in the upstream shape;
//! * `thinking: disabled` becomes the only recognised off switch;
//! * a request that enables thinking without an effort asks for the reference's
//!   preferred tier (`high`);
//! * a silent caller gets neither field, so its behaviour is unchanged.
//!
//! Deliberate omissions (see the migration document):
//! * no system-prompt folding, no channel-marker rewriting, no desensitisation;
//! * assistant `reasoning_content` is not round-tripped: the field is observed
//!   on the response side only, and sending it would be an invented input field.
//!
//! `developer` is normalised to `system` because the upstream rejects the
//! OpenAI `developer` role; that is role mapping, not content editing.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::error::GatewayError;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, Usage,
};
use crate::providers::{is_image_media, openai_content};

pub fn build_body(request: &ChatRequest) -> Result<Value, GatewayError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert(
        "messages".into(),
        Value::Array(messages_to_upstream(&request.messages)?),
    );
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));
    if !request.tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools_to_upstream(request)));
    }
    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_upstream(choice));
    }
    if let Some(max_tokens) = request.max_output_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
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
    apply_thinking(&mut body, request);
    Ok(Value::Object(body))
}

/// Spellings a caller may use to mean "do not think". The upstream rejects
/// `reasoning_effort: "off"` with `11150` and treats `"none"` as *enable*, so
/// the only safe translation is `thinking.disabled`.
const THINKING_OFF_SPELLINGS: [&str; 5] = ["off", "none", "disabled", "false", "0"];

fn apply_thinking(body: &mut Map<String, Value>, request: &ChatRequest) {
    let effort = request
        .reasoning_effort
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(effort) = effort {
        if THINKING_OFF_SPELLINGS.contains(&effort.as_str()) {
            body.insert("thinking".into(), json!({ "type": "disabled" }));
        } else {
            body.insert("reasoning_effort".into(), json!(effort));
        }
        return;
    }
    match request.enable_thinking {
        Some(false) => {
            body.insert("thinking".into(), json!({ "type": "disabled" }));
        }
        // The caller enabled reasoning without naming a tier. The reference
        // picks its preferred tier (`high`) in this case; the upstream rejects
        // a tier the model does not support, so this is the narrowest guess.
        Some(true) => {
            body.insert("reasoning_effort".into(), json!("high"));
        }
        None => {}
    }
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    let mut out = Vec::with_capacity(messages.len());
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut previous_assistant = false;

    for message in messages {
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
                if message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                    )
                }) {
                    return Err(GatewayError::new(
                        crate::error::ErrorKind::Unsupported,
                        "a `tool` message cannot carry image content",
                    ));
                }
                for result in tool_results {
                    if !declared.contains(&result.tool_call_id) {
                        return Err(GatewayError::bad_request(format!(
                            "tool result `{}` does not belong to a preceding assistant tool call",
                            result.tool_call_id
                        )));
                    }
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_call_id,
                        "content": result.content,
                    }));
                }
                previous_assistant = false;
            }
            MessageRole::Assistant => {
                if !tool_results.is_empty() {
                    return Err(GatewayError::bad_request(
                        "an `assistant` message cannot carry a tool result",
                    ));
                }
                if message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                    )
                }) {
                    return Err(GatewayError::new(
                        crate::error::ErrorKind::Unsupported,
                        "an assistant message cannot carry image content",
                    ));
                }
                let mut object = Map::new();
                object.insert("role".into(), json!("assistant"));
                if let Some(name) = &message.name {
                    object.insert("name".into(), json!(name));
                }
                object.insert("content".into(), json!(text));
                if !tool_calls.is_empty() {
                    let ids: BTreeSet<String> = tool_calls
                        .iter()
                        .map(|call| call.id.clone())
                        .filter(|id| !id.is_empty())
                        .collect();
                    declared = if previous_assistant {
                        declared.union(&ids).cloned().collect()
                    } else {
                        ids
                    };
                    object.insert(
                        "tool_calls".into(),
                        Value::Array(
                            tool_calls
                                .iter()
                                .map(|call| tool_call_to_upstream(call))
                                .collect(),
                        ),
                    );
                } else {
                    declared.clear();
                }
                out.push(Value::Object(object));
                previous_assistant = true;
            }
            role => {
                if !tool_calls.is_empty() || !tool_results.is_empty() {
                    return Err(GatewayError::bad_request(
                        "only an assistant message may declare tool calls",
                    ));
                }
                declared.clear();
                previous_assistant = false;
                let content = openai_content("WorkBuddy", &message.content)?;
                let mut object = Map::new();
                object.insert("role".into(), json!(role_name(role)));
                if let Some(name) = &message.name {
                    object.insert("name".into(), json!(name));
                }
                object.insert("content".into(), content);
                out.push(Value::Object(object));
            }
        }
    }
    Ok(out)
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

    fn assistant_with_call() -> ChatMessage {
        let mut message = ChatMessage::new(MessageRole::Assistant, vec![ContentBlock::text("hi")]);
        message.content.push(ContentBlock::ToolCall(ToolCall::new(
            "call-1", "lookup", "{}",
        )));
        message
    }

    #[test]
    fn developer_role_is_sent_as_system() {
        let request = ChatRequest::new(
            "glm-5.2",
            vec![ChatMessage::new(
                MessageRole::Developer,
                vec![ContentBlock::text("rules")],
            )],
        );
        let body = build_body(&request).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn tool_results_must_follow_their_call() {
        let orphan = ChatRequest::new(
            "m",
            vec![ChatMessage::new(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult(ToolResult::new("call-1", "ok"))],
            )],
        );
        let error = build_body(&orphan).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::BadRequest);

        let paired = ChatRequest::new(
            "m",
            vec![
                assistant_with_call(),
                ChatMessage::new(
                    MessageRole::Tool,
                    vec![ContentBlock::ToolResult(ToolResult::new("call-1", "ok"))],
                ),
            ],
        );
        let body = build_body(&paired).unwrap();
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call-1");
    }

    #[test]
    fn tools_and_choice_are_forwarded() {
        let mut request = ChatRequest::new("m", vec![]);
        request.tools.push(ToolDefinition {
            name: "lookup".into(),
            description: Some("find".into()),
            parameters: None,
        });
        request.tool_choice = Some(ToolChoice::Required);
        let body = build_body(&request).unwrap();
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn caller_effort_is_forwarded_flat_and_lowercased() {
        let mut request = ChatRequest::new("m", vec![]);
        request.reasoning_effort = Some(" High ".into());
        let body = build_body(&request).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn disabling_thinking_uses_the_only_off_switch() {
        for effort in ["off", "none", "disabled", "false", "0"] {
            let mut request = ChatRequest::new("m", vec![]);
            request.reasoning_effort = Some(effort.into());
            let body = build_body(&request).unwrap();
            assert!(body.get("reasoning_effort").is_none(), "{effort}");
            assert_eq!(body["thinking"]["type"], "disabled", "{effort}");
        }
        let mut request = ChatRequest::new("m", vec![]);
        request.enable_thinking = Some(false);
        let body = build_body(&request).unwrap();
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn enabling_thinking_without_an_effort_picks_the_reference_tier() {
        let mut request = ChatRequest::new("m", vec![]);
        request.enable_thinking = Some(true);
        let body = build_body(&request).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn a_silent_caller_still_sends_no_thinking_fields() {
        let body = build_body(&ChatRequest::new("m", vec![])).unwrap();
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("thinking").is_none());
    }
}
