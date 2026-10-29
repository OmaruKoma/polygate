//! Wire-API abstraction shared by the three public surfaces.
//!
//! `POST /v1/chat/completions`, `POST /v1/responses` and `POST /v1/messages`
//! have different request/response shapes but share one pipeline: resolve the
//! exposed model → scheduler/session → provider → internal protocol. This trait
//! is the only place the API layer knows about a specific client dialect.

use serde_json::Value;

use crate::error::GatewayError;
use crate::model_service::RouteNeeds;
use crate::protocol::{ChatRequest, ChatResponse};

use super::sse::SseTranslator;

pub(crate) trait WireRequest {
    /// The model id exactly as the client sent it (possibly an alias).
    fn model(&self) -> &str;
    fn streaming(&self) -> bool;
    /// Reject parameters the gateway cannot honour.
    fn validate(&self) -> Result<(), GatewayError>;
    /// Capabilities the resolved target must declare.
    fn route_needs(&self) -> RouteNeeds;
    /// Value stored in `request_logs.api`.
    fn api_name(&self) -> &'static str;
    /// First user text, used for the automatic session id when the client did
    /// not send `x-session-id`.
    fn first_user_text(&self) -> Option<String>;
    /// Convert to the internal request for a specific upstream model id.
    fn to_chat_request(
        &self,
        upstream_model: &str,
        request_id: String,
    ) -> Result<ChatRequest, GatewayError>;
    /// Streaming state machine for this dialect.
    fn translator(&self, exposed_model: String) -> Box<dyn SseTranslator>;
    /// Non-streaming response body in this dialect.
    fn render(&self, response: &ChatResponse, exposed_model: &str) -> Result<Value, GatewayError>;
}

/// Vision detection shared by the wire adapters: any content block that is an
/// image, or carries an image media type, requires a vision-capable target.
pub(crate) fn content_has_image(content: &Value) -> bool {
    match content {
        Value::Array(blocks) => blocks.iter().any(block_has_image),
        _ => false,
    }
}

pub(crate) fn block_has_image(block: &Value) -> bool {
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind == "image_url" || kind == "image" || kind == "input_image" {
        return true;
    }
    block
        .get("media_type")
        .and_then(Value::as_str)
        .is_some_and(|media| media.starts_with("image/"))
        || block
            .get("source")
            .and_then(|source| source.get("media_type"))
            .and_then(Value::as_str)
            .is_some_and(|media| media.starts_with("image/"))
}
