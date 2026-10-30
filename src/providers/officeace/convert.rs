//! Internal protocol <-> OfficeAce model-gateway conversion.
//!
//! The gateway speaks the standard OpenAI chunk shape, so this module is a
//! projection in both directions: request body out, streamed chunks in.
//!
//! Two upstream behaviours are encoded here:
//!
//! * `max_tokens` is the **total** budget (reasoning + answer) and must carry
//!   headroom, otherwise reasoning models return an empty answer;
//! * `max_completion_tokens` is normalized to `max_tokens`, the only name the
//!   upstream understands.

use serde_json::{json, Map, Value};

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamEvent,
    ToolCall, ToolChoice, Usage,
};
use crate::providers::{is_image_media, openai_content};

use super::region;

pub fn build_body(request: &ChatRequest) -> Result<Value, GatewayError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert(
        "messages".into(),
        json!(messages_to_upstream(&request.messages)?),
    );
    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(max_tokens) = request.max_output_tokens {
        body.insert(
            "max_tokens".into(),
            json!(region::clamp_max_tokens(max_tokens, &request.model)),
        );
    }
    if !request.stop.is_empty() {
        body.insert("stop".into(), json!(request.stop));
    }
    if !request.tools.is_empty() {
        body.insert("tools".into(), json!(tools_to_upstream(request)?));
    }
    if let Some(choice) = &request.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_upstream(choice));
    }
    Ok(Value::Object(body))
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    messages.iter().map(message_to_upstream).collect()
}

fn message_to_upstream(message: &ChatMessage) -> Result<Value, GatewayError> {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();

    match message.role {
        MessageRole::Tool => {
            let result = message.tool_results().next().ok_or_else(|| {
                GatewayError::bad_request("a `tool` message must carry a tool result")
            })?;
            if message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                )
            }) {
                return Err(GatewayError::new(
                    ErrorKind::Unsupported,
                    "a `tool` message cannot carry image content",
                ));
            }
            Ok(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": result.content,
            }))
        }
        role => {
            let tool_calls = message
                .tool_calls()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })
                })
                .collect::<Vec<_>>();
            let content = if tool_calls.is_empty() {
                openai_content("OfficeAce", &message.content)?
            } else {
                if message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type)
                    )
                }) {
                    return Err(GatewayError::new(
                        ErrorKind::Unsupported,
                        "an assistant tool-call message cannot carry image content",
                    ));
                }
                if text.is_empty() {
                    json!("")
                } else {
                    json!(text)
                }
            };
            let mut object = Map::new();
            object.insert("role".into(), json!(role_name(role)));
            if let Some(name) = &message.name {
                object.insert("name".into(), json!(name));
            }
            object.insert("content".into(), content);
            if !tool_calls.is_empty() {
                object.insert("tool_calls".into(), json!(tool_calls));
            }
            Ok(Value::Object(object))
        }
    }
}

fn tools_to_upstream(request: &ChatRequest) -> Result<Vec<Value>, GatewayError> {
    let mut tools = Vec::with_capacity(request.tools.len());
    for tool in &request.tools {
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
        tools.push(json!({"type": "function", "function": function}));
    }
    Ok(tools)
}

fn tool_choice_to_upstream(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub fn parse_usage(value: &Value) -> Usage {
    let input = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let output = value
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: value
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        cached_input_tokens: value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64),
        total_tokens: value.get("total_tokens").and_then(Value::as_u64),
    }
}

/// Fold a fully consumed stream into one response.
pub fn aggregate(
    id: impl Into<String>,
    request_id: Option<String>,
    model: impl Into<String>,
    events: &[StreamEvent],
) -> ChatResponse {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut finish = FinishReason::Unknown;
    let mut usage = None;

    for event in events {
        match event {
            StreamEvent::TextDelta { text: delta } => text.push_str(delta),
            StreamEvent::ReasoningDelta { text: delta } => reasoning.push_str(delta),
            StreamEvent::ToolCallStart {
                id: call_id, name, ..
            } => {
                if !tool_calls.iter().any(|call| &call.id == call_id) {
                    tool_calls.push(ToolCall {
                        id: call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    });
                }
            }
            StreamEvent::ToolCallArgsDelta { id: call_id, delta } => {
                if let Some(call) = tool_calls.iter_mut().find(|call| &call.id == call_id) {
                    call.arguments.push_str(delta);
                }
            }
            StreamEvent::Usage { usage: reported } => usage = Some(*reported),
            StreamEvent::Finish { reason } => finish = reason.clone(),
            StreamEvent::Error { .. } | StreamEvent::Start { .. } => {}
        }
    }

    let mut message = ChatMessage::new(MessageRole::Assistant, Vec::with_capacity(3));
    if !reasoning.is_empty() {
        message
            .content
            .push(ContentBlock::Reasoning { text: reasoning });
    }
    if !text.is_empty() {
        message.content.push(ContentBlock::Text { text });
    }
    message
        .content
        .extend(tool_calls.into_iter().map(ContentBlock::ToolCall));

    let mut response = ChatResponse::new(id, model, message, finish);
    response.request_id = request_id;
    response.usage = usage;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChatMessage, MessageRole, ToolDefinition};

    #[test]
    fn a_request_maps_onto_the_openai_body_with_reasoning_headroom() {
        let mut request = ChatRequest::new(
            "glm-5.2",
            vec![ChatMessage::system("be brief"), ChatMessage::user("hi")],
        );
        request.max_output_tokens = Some(100);
        request.temperature = Some(0.2);
        request.tools = vec![ToolDefinition::new(
            "get_weather",
            json!({"type": "object"}),
        )];
        let body = build_body(&request).expect("body");
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_tokens"], json!(1124));
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["tools"][0]["function"]["name"], json!("get_weather"));
    }

    #[test]
    fn a_tiny_cap_is_forwarded_unchanged() {
        let mut request = ChatRequest::new("glm-5.2", vec![ChatMessage::user("hi")]);
        request.max_output_tokens = Some(4);
        assert_eq!(build_body(&request).expect("body")["max_tokens"], json!(4));
    }

    #[test]
    fn an_assistant_tool_call_serializes_null_content_as_an_empty_string() {
        let mut message = ChatMessage::new(MessageRole::Assistant, Vec::new());
        message.content.push(ContentBlock::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }));
        let value = message_to_upstream(&message).expect("message");
        assert_eq!(value["content"], json!(""));
        assert_eq!(value["tool_calls"][0]["id"], json!("call_1"));
    }

    #[test]
    fn unsupported_media_is_rejected_instead_of_dropped() {
        let mut message = ChatMessage::user("look");
        message.content.push(ContentBlock::Unsupported {
            media_type: "image/png".into(),
            data: json!({}),
        });
        let error = message_to_upstream(&message).expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn a_stream_aggregates_text_reasoning_tool_calls_and_usage() {
        let events = vec![
            StreamEvent::ReasoningDelta {
                text: "think".into(),
            },
            StreamEvent::TextDelta { text: "hel".into() },
            StreamEvent::TextDelta { text: "lo".into() },
            StreamEvent::ToolCallStart {
                id: "c1".into(),
                name: "shell".into(),
                index: Some(0),
            },
            StreamEvent::ToolCallArgsDelta {
                id: "c1".into(),
                delta: "{\"a\":".into(),
            },
            StreamEvent::ToolCallArgsDelta {
                id: "c1".into(),
                delta: "1}".into(),
            },
            StreamEvent::Usage {
                usage: Usage::new(3, 4),
            },
            StreamEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ];
        let response = aggregate("r1", None, "glm-5.2", &events);
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.message.text_content(), "hello");
        let call = response.message.tool_calls().next().expect("tool call");
        assert_eq!(call.arguments, "{\"a\":1}");
        assert_eq!(response.usage.expect("usage").total(), 7);
    }
}
