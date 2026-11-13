//! Doubao `/chat/completion` request construction.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md` §3-§4 plus a live probe on 2026-11-13.
//!
//! The upstream takes one `messages[0]` whose `content_block` array carries the
//! whole turn. It does not accept the OpenAI message array, and it does not
//! route by model name — the thinking tier is the only dial
//! (`option.need_deep_think` / `ext.use_deep_think`).
//!
//! Deliberate omissions:
//! * no system prompt wrapper or marker rewriting; history is rendered as plain
//!   turns, which is what the client already sent;
//! * no `answer_with_suggest = false` (measured: the upstream then stops
//!   emitting the reasoning chain);
//! * no skill invocation (`action_bar_skill_id` stays `0`);
//! * no image/file attachments yet — the upstream only accepts its own resource
//!   centre `uri`, so forwarding needs the `prepare_upload` + imageX transfer
//!   path, which this adapter does not implement. Image input is reported as
//!   unsupported instead of being silently dropped.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatMessage, ChatRequest, ContentBlock, MessageRole};
use crate::providers::is_image_media;

use super::region;

const BLOCK_TEXT: i64 = 10_000;
/// Thinking container block. The first container is the reasoning chain, the
/// second one is the visible answer.
pub const BLOCK_THINKING: i64 = 10_040;

/// One flattened turn: the whole conversation rendered as a single prompt.
#[derive(Debug, Default)]
pub struct Turn {
    pub text: String,
}

impl Turn {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// Render the OpenAI history into one upstream prompt.
///
/// The upstream keeps its own conversation state, but OpenAI clients resend the
/// whole history every turn; flattening keeps both behaviours equivalent
/// without depending on a server-side conversation id.
pub fn flatten(messages: &[ChatMessage]) -> Result<Turn, GatewayError> {
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<String> = Vec::new();

    for message in messages {
        let mut text = String::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text: part } => text.push_str(part),
                ContentBlock::Unsupported { media_type, .. } if is_image_media(media_type) => {
                    return Err(GatewayError::new(
                        ErrorKind::Unsupported,
                        "Doubao adapter does not support image input yet",
                    ));
                }
                ContentBlock::Unsupported { media_type, .. } => {
                    return Err(GatewayError::new(
                        ErrorKind::Unsupported,
                        format!("Doubao adapter cannot forward `{media_type}` content"),
                    ));
                }
                ContentBlock::Reasoning { .. } => {}
                ContentBlock::ToolCall(_) | ContentBlock::ToolResult(_) => {
                    return Err(GatewayError::bad_request(
                        "Doubao chat has no tool-calling surface; send tool output as user text",
                    ));
                }
            }
        }
        let text = text.trim().to_string();
        match message.role {
            MessageRole::System | MessageRole::Developer => {
                if !text.is_empty() {
                    system.push(text);
                }
            }
            MessageRole::User => {
                if !text.is_empty() {
                    turns.push(text);
                }
            }
            MessageRole::Assistant => {
                if !text.is_empty() {
                    turns.push(format!("Assistant: {text}"));
                }
            }
            MessageRole::Tool => {
                return Err(GatewayError::bad_request(
                    "Doubao chat has no tool-calling surface; send tool output as user text",
                ));
            }
        }
    }

    let mut prompt = String::new();
    let mut has_system = false;
    for line in system {
        prompt.push_str(&line);
        prompt.push('\n');
        has_system = true;
    }
    if has_system && !turns.is_empty() {
        prompt.push('\n');
    }
    prompt.push_str(&turns.join("\n"));
    Ok(Turn { text: prompt })
}

/// Thinking tier for the requested public model. Unknown names fall back to the
/// fast tier, matching the upstream's own behaviour (it has no model router).
pub fn think_level(model: &str) -> i64 {
    region::tier_for_model(model).unwrap_or(region::THINK_OFF)
}

/// Build the `/chat/completion` body.
pub fn build_body(request: &ChatRequest, turn: &Turn) -> Result<Value, GatewayError> {
    if turn.is_empty() {
        return Err(GatewayError::bad_request("Doubao request has no text"));
    }
    let local_conversation = format!("local_{}", &Uuid::new_v4().simple().to_string()[..8]);
    let work = region::work_model(&request.model);
    let tier = match work {
        Some(_) => region::WORK_THINK,
        None => think_level(&request.model),
    };

    let mut option = Map::new();
    option.insert("send_message_scene".into(), json!(""));
    option.insert("create_time_ms".into(), json!(0));
    option.insert("collect_id".into(), json!(""));
    option.insert("is_audio".into(), json!(false));
    // Measured: `false` makes the upstream drop the reasoning chain entirely.
    option.insert("answer_with_suggest".into(), json!(true));
    option.insert("tts_switch".into(), json!(false));
    option.insert("need_deep_think".into(), json!(tier));
    option.insert("click_clear_context".into(), json!(false));
    option.insert("from_suggest".into(), json!(false));
    option.insert("is_regen".into(), json!(false));
    option.insert("is_replace".into(), json!(false));
    option.insert("disable_sse_cache".into(), json!(false));
    option.insert("select_text_action".into(), json!(""));
    option.insert("resend_for_regen".into(), json!(false));
    option.insert("scene_type".into(), json!(0));
    option.insert("unique_key".into(), json!(Uuid::new_v4().to_string()));
    option.insert("start_seq".into(), json!(0));
    option.insert("need_create_conversation".into(), json!(true));
    option.insert(
        "conversation_init_option".into(),
        json!({"need_ack_conversation": true}),
    );
    option.insert("regen_query_id".into(), json!([]));
    option.insert("edit_query_id".into(), json!([]));
    option.insert("regen_instruction".into(), json!(""));
    option.insert("no_replace_for_regen".into(), json!(false));
    option.insert("message_from".into(), json!(0));
    option.insert("shared_app_name".into(), json!(""));
    option.insert("action_bar_skill_id".into(), json!(0));
    option.insert(
        "sse_recv_event_options".into(),
        json!({"support_chunk_delta": true}),
    );
    option.insert("is_ai_playground".into(), json!(false));

    // The client selects a work model in three places at once; the upstream
    // resolves the model from `model_config`. `need_deep_think = 10001` is the
    // menu type the client sends for every work-model entry.
    if let Some(model) = work {
        option.insert("agent_mode".into(), json!(1));
        option.insert(
            "conversation_init_ext".into(),
            json!({ "model_item_key": model.key }),
        );
        option.insert(
            "model_config".into(),
            json!({
                "model_item_key": model.key,
                "model_extra_params": {
                    "memory_profile": "300k_230",
                    "total_window_size": model.window.to_string(),
                },
                "reasoning_effort": 5,
            }),
        );
    }

    Ok(json!({
        "client_meta": {
            "local_conversation_id": local_conversation,
            "conversation_id": "0",
            "bot_id": region::DEFAULT_BOT_ID,
            "last_section_id": "",
            "last_message_index": 0,
        },
        "messages": [{
            "local_message_id": Uuid::new_v4().to_string(),
            "content_block": [text_block(&turn.text)],
            "message_status": 0,
        }],
        "option": Value::Object(option),
        "chat_ability": {},
        "ext": {
            "use_deep_think": tier.to_string(),
            "use_submit_pipeline": "1",
            "commerce_credit_config_enable": "0",
            "sub_conv_firstmet_type": "1",
        },
    }))
}

fn text_block(text: &str) -> Value {
    json!({
        "block_type": BLOCK_TEXT,
        "content": {
            "text_block": {
                "text": text,
                "icon_url": "",
                "icon_url_dark": "",
                "summary": "",
            },
            "pc_event_block": "",
        },
        "block_id": Uuid::new_v4().to_string(),
        "parent_id": "",
        "meta_info": [],
        "append_fields": [],
        "is_finish": true,
        "patch_type": 2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ChatMessage, ChatRequest, ContentBlock};

    fn message(role: MessageRole, text: &str) -> ChatMessage {
        ChatMessage::new(role, vec![ContentBlock::text(text)])
    }

    #[test]
    fn history_is_flattened_without_losing_the_system_prompt() {
        let turn = flatten(&[
            message(MessageRole::System, "be terse"),
            message(MessageRole::User, "hi"),
            message(MessageRole::Assistant, "hello"),
            message(MessageRole::User, "again"),
        ])
        .expect("flatten");
        assert_eq!(turn.text, "be terse\n\nhi\nAssistant: hello\nagain");
    }

    #[test]
    fn images_are_refused_instead_of_being_dropped() {
        let with_image = ChatMessage::new(
            MessageRole::User,
            vec![
                ContentBlock::text("what is this"),
                ContentBlock::Unsupported {
                    media_type: "image_url".into(),
                    data: serde_json::json!({"type": "image_url"}),
                },
            ],
        );
        let error = flatten(&[with_image]).expect_err("image");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("image"));
    }

    #[test]
    fn tool_messages_are_refused_rather_than_silently_flattened() {
        let error = flatten(&[message(MessageRole::Tool, "ok")]).expect_err("tool");
        assert_eq!(error.kind, ErrorKind::BadRequest);
    }

    #[test]
    fn the_thinking_tier_follows_the_public_model_name() {
        let request = ChatRequest::new("doubao-expert", vec![]);
        let body = build_body(&request, &Turn { text: "hi".into() }).expect("body");
        assert_eq!(body["option"]["need_deep_think"], 3);
        assert_eq!(body["ext"]["use_deep_think"], "3");

        let request = ChatRequest::new("unknown", vec![]);
        let body = build_body(&request, &Turn { text: "hi".into() }).expect("body");
        assert_eq!(body["option"]["need_deep_think"], 0);
    }

    #[test]
    fn a_work_model_carries_the_menu_type_and_model_key() {
        let request = ChatRequest::new("doubao-glm-5.3", vec![]);
        let body = build_body(&request, &Turn { text: "hi".into() }).expect("body");
        let option = &body["option"];
        assert_eq!(option["need_deep_think"], 10_001);
        assert_eq!(option["agent_mode"], 1);
        assert_eq!(
            option["model_config"]["model_item_key"],
            "person-glm-5-3-261111"
        );
        assert_eq!(
            option["conversation_init_ext"]["model_item_key"],
            "person-glm-5-3-261111"
        );
        assert_eq!(
            option["model_config"]["model_extra_params"]["total_window_size"],
            "300000"
        );
        assert_eq!(body["ext"]["use_deep_think"], "10001");
    }

    #[test]
    fn a_plain_tier_does_not_carry_work_fields() {
        let request = ChatRequest::new("doubao", vec![]);
        let body = build_body(&request, &Turn { text: "hi".into() }).expect("body");
        assert!(body["option"].get("model_config").is_none());
        assert!(body["option"].get("agent_mode").is_none());
    }

    #[test]
    fn an_empty_prompt_is_a_local_error() {
        let error =
            build_body(&ChatRequest::new("doubao", vec![]), &Turn::default()).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::BadRequest);
    }

    #[test]
    fn the_fields_the_upstream_requires_are_present() {
        let body = build_body(
            &ChatRequest::new("doubao", vec![]),
            &Turn { text: "hi".into() },
        )
        .expect("body");
        assert_eq!(body["client_meta"]["bot_id"], region::DEFAULT_BOT_ID);
        assert_eq!(body["option"]["answer_with_suggest"], true);
        assert_eq!(
            body["messages"][0]["content_block"][0]["block_type"],
            BLOCK_TEXT
        );
        assert_eq!(
            body["option"]["sse_recv_event_options"]["support_chunk_delta"],
            true
        );
    }
}
