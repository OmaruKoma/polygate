//! Loomy model catalogue parsing.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md` §4. `GET /models` returns an
//! OpenAI-shaped list whose entries carry `context_length`, `max_output_tokens`
//! and a `capabilities` object. Model ids are used verbatim upstream.

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::providers::UpstreamModel;

pub fn parse_catalog(value: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let items = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .ok_or_else(|| {
            GatewayError::new(
                ErrorKind::UpstreamServer,
                "Loomy model catalogue has no `data` array",
            )
        })?;

    let mut models = Vec::with_capacity(items.len());
    for item in items {
        // The catalogue mixes chat and media models; only chat entries can be
        // served through `/chat/completions`. An entry without `type` is kept.
        if let Some(kind) = item.get("type").and_then(Value::as_str) {
            if !kind.eq_ignore_ascii_case("chat") {
                continue;
            }
        }
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let display_name = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let context_window = item
            .get("context_length")
            .and_then(Value::as_u64)
            .or_else(|| item.get("context_window").and_then(Value::as_u64));
        let max_output = item
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .or_else(|| item.get("max_output").and_then(Value::as_u64));
        let mut model = UpstreamModel::new(id);
        if let Some(name) = display_name {
            model = model.with_display_name(name);
        }
        model.context_window = context_window;
        model.max_output = max_output;
        models.push(model);
    }

    if models.is_empty() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "Loomy model catalogue is empty",
        ));
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_openai_catalogue_shape_is_parsed() {
        let models = parse_catalog(&json!({
            "object": "list",
            "data": [
                {"id": "deepseek-v4-flash-0731", "name": "DeepSeek V4 Flash (x0.3)", "type": "chat",
                 "context_length": 262144, "max_output_tokens": 32768,
                 "capabilities": {"reasoning": true, "vision": false}},
                {"id": "spark-x", "name": "Spark X (x0.1)", "type": "chat"},
                {"id": "qwen-image-3.0-pro", "name": "Qwen Image", "type": "image"}
            ]
        }))
        .expect("parse");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-v4-flash-0731");
        assert_eq!(models[0].context_window, Some(262144));
        assert_eq!(models[0].max_output, Some(32768));
        assert_eq!(
            models[0].display_name.as_deref(),
            Some("DeepSeek V4 Flash (x0.3)")
        );
    }

    #[test]
    fn entries_without_an_id_are_skipped() {
        let models = parse_catalog(&json!({"data": [{"name": "x"}, {"id": "ok"}]})).expect("parse");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ok");
    }

    #[test]
    fn a_body_without_a_catalogue_is_an_upstream_error() {
        let error = parse_catalog(&json!({"success": false})).expect_err("no catalogue");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }
}
