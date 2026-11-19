//! TeleAgent model catalogue parsing.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md` §3. `GET /model-options` answers
//! `{"code":0,"data":{"models":[{"model":"chat-pro", …}]}}` with a display
//! name, a description and a `points_multiplier`; `context_limit` is `null` for
//! every entry, so only the documented chat-pro figures are filled in.
//!
//! Ids are used verbatim upstream (`chat-pro` / `chat-flagship` / `chat-flash`).

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::providers::UpstreamModel;

use super::region;

pub fn parse_catalog(value: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let items = value
        .pointer("/data/models")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .ok_or_else(|| {
            GatewayError::new(
                ErrorKind::UpstreamServer,
                "TeleAgent model catalogue has no `data.models` array",
            )
        })?;

    let mut models = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item.get("model").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let display_name = item
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let mut model = UpstreamModel::new(id);
        if let Some(name) = display_name {
            model = model.with_display_name(name);
        }
        model.context_window = item.get("context_limit").and_then(Value::as_u64);
        if id == "chat-pro" && model.context_window.is_none() {
            model.context_window = Some(region::CHAT_PRO_CONTEXT);
            model.max_output = Some(region::CHAT_PRO_MAX_OUTPUT);
        }
        models.push(model);
    }

    if models.is_empty() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "TeleAgent model catalogue is empty",
        ));
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_catalogue_maps_ids_and_the_documented_window() {
        let models = parse_catalog(&json!({
            "code": 0,
            "data": {
                "models": [
                    {"model": "chat-pro", "display_name": "智能", "is_default": true, "context_limit": null},
                    {"model": "chat-flash", "display_name": "极速", "context_limit": null}
                ]
            }
        }))
        .expect("catalogue");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "chat-pro");
        assert_eq!(models[0].display_name.as_deref(), Some("智能"));
        assert_eq!(models[0].context_window, Some(400_000));
        assert_eq!(models[0].max_output, Some(64_000));
        assert_eq!(models[1].id, "chat-flash");
        assert_eq!(models[1].context_window, None);
    }

    #[test]
    fn an_empty_catalogue_is_an_upstream_error() {
        let error = parse_catalog(&json!({"code": 0, "data": {"models": []}})).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }
}
