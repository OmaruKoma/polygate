//! Internal protocol <-> Loomy `/chat/completions` conversion.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md` §4. The upstream speaks OpenAI chat
//! completions in both directions, so this adapter is mostly a faithful
//! projection: text, tool calls, tool results and the usual sampling knobs.
//!
//! Deliberate omissions:
//! * no `enable_thinking` / `/no_think` injection — that is a client behaviour,
//!   not a gateway default;
//! * assistant `reasoning_content` is observed on responses and never sent back
//!   as an input field;
//! * `developer` messages are sent unchanged (the upstream accepts them).
//!
//! `points_consumed` in `usage` is an upstream extension; it is kept in the
//! response `raw` for accounting but never mapped onto token counts.

use serde_json::{json, Map, Value};

use crate::error::GatewayError;
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, ToolCall,
    ToolChoice, Usage,
};
use crate::providers::{is_image_media, openai_content};

pub fn build_body(request: &ChatRequest, stream: bool) -> Result<Value, GatewayError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert(
        "messages".into(),
        Value::Array(messages_to_upstream(&request.messages)?),
    );
    body.insert("stream".into(), json!(stream));
    if stream {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
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
    Ok(Value::Object(body))
}

fn messages_to_upstream(messages: &[ChatMessage]) -> Result<Vec<Value>, GatewayError> {
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        match message.role {
            MessageRole::Tool => {
                let results: Vec<_> = message.tool_results().collect();
                if results.is_empty() {
                    return Err(GatewayError::bad_request(
                        "a `tool` message must carry a tool result",
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
                for result in results {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_call_id,
                        "content": result.content,
                    }));
                }
            }
            MessageRole::Assistant => {
                let text: String = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let calls: Vec<&ToolCall> = message.tool_calls().collect();
                let mut object = Map::new();
                object.insert("role".into(), json!("assistant"));
                if let Some(name) = &message.name {
                    object.insert("name".into(), json!(name));
                }
                object.insert("content".into(), json!(text));
                if !calls.is_empty() {
                    let calls: Vec<Value> = calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {"name": call.name, "arguments": call.arguments},
                            })
                        })
                        .collect();
                    object.insert("tool_calls".into(), Value::Array(calls));
                }
                out.push(Value::Object(object));
            }
            role => {
                let content = openai_content("Loomy", &message.content)?;
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

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Developer => "developer",
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
            if let Some(parameters) = &tool.parameters {
                function.insert("parameters".into(), parameters.clone());
            }
            json!({"type": "function", "function": Value::Object(function)})
        })
        .collect()
}

fn tool_choice_to_upstream(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

pub fn parse_response(
    id: String,
    model: &str,
    value: &Value,
) -> Result<ChatResponse, GatewayError> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| {
            GatewayError::new(
                crate::error::ErrorKind::UpstreamServer,
                "Loomy response has no choices",
            )
        })?;
    let message = choice.get("message").cloned().unwrap_or(Value::Null);

    let mut content = Vec::new();
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
        if !reasoning.is_empty() {
            content.push(ContentBlock::reasoning(reasoning));
        }
    }
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(ContentBlock::text(text));
        }
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            content.push(ContentBlock::ToolCall(ToolCall::new(
                id,
                name,
                arguments.to_string(),
            )));
        }
    }

    let finish = FinishReason::from_upstream(
        choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let usage = value.get("usage").map(parse_usage);
    let model_name = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(model)
        .to_string();

    Ok(ChatResponse {
        id,
        request_id: None,
        model: model_name,
        message: ChatMessage::new(MessageRole::Assistant, content),
        finish_reason: finish,
        usage,
        created_at: chrono::Utc::now(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChatMessage, MessageRole, ToolDefinition};

    #[test]
    fn a_request_maps_onto_the_openai_body() {
        let mut request = ChatRequest::new(
            "deepseek-v4-flash-0731",
            vec![ChatMessage::system("be brief"), ChatMessage::user("hi")],
        );
        request.max_output_tokens = Some(32);
        request.temperature = Some(0.2);
        request.tools = vec![ToolDefinition::new(
            "get_weather",
            json!({"type": "object"}),
        )];
        request.tool_choice = Some(ToolChoice::Auto);

        let body = build_body(&request, true).expect("body");
        assert_eq!(body["model"], "deepseek-v4-flash-0731");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 32);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn unsupported_blocks_are_rejected_by_name() {
        let message = ChatMessage::new(
            MessageRole::User,
            vec![ContentBlock::Unsupported {
                media_type: "image/png".into(),
                data: json!({}),
            }],
        );
        let error = build_body(&ChatRequest::new("m", vec![message]), false).expect_err("reject");
        assert!(error.message.contains("image/png"));
    }

    #[test]
    fn a_response_carries_reasoning_text_and_tool_calls() {
        let response = parse_response(
            "id-1".into(),
            "m",
            &json!({
                "model": "deepseek-v4-flash-0731",
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "content": "checking",
                        "reasoning_content": "think",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14,
                          "completion_tokens_details": {"reasoning_tokens": 2}}
            }),
        )
        .expect("parse");
        assert_eq!(response.message.content.len(), 3);
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.usage.expect("usage").reasoning_tokens, Some(2));
    }

    #[test]
    fn a_response_without_choices_is_an_upstream_error() {
        let error = parse_response("id".into(), "m", &json!({"choices": []})).expect_err("empty");
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }
}
