//! CodeArts built-in model discovery.
//!
//! Endpoint: `GET {snap}/v1/model/builtin` with `Agent-Type: PromptCenter`
//! (source: local `app/codearts/model_catalog.py`, Coding2API
//! `client.py::_fetch_builtin_models`).
//!
//! Only models actually present in the upstream response are reported. The
//! adapter never falls back to a hard-coded model list.

use serde_json::Value;

use crate::providers::UpstreamModel;

/// Parse the `builtinModels` array.
pub fn parse_models(payload: &Value) -> Vec<UpstreamModel> {
    let items = payload
        .get("builtinModels")
        .and_then(Value::as_array)
        .or_else(|| {
            payload
                .get("data")
                .and_then(|data| data.get("builtinModels"))
                .and_then(Value::as_array)
        })
        .or_else(|| payload.get("models").and_then(Value::as_array));
    let Some(items) = items else {
        return Vec::new();
    };

    let mut models: Vec<UpstreamModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let id = ["model_id", "modelId", "id", "model_name", "name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(id) = id else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let display = ["model_name", "modelName", "name", "display_name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != id)
            .map(str::to_string);
        let context_window = ["context_window", "contextWindow", "max_input_tokens"]
            .iter()
            .find_map(|key| positive_u64(object.get(*key)));
        let max_output = ["max_tokens", "maxTokens", "max_output_tokens"]
            .iter()
            .find_map(|key| positive_u64(object.get(*key)));

        let mut model = UpstreamModel::new(id);
        model.display_name = display;
        model.context_window = context_window;
        model.max_output = max_output;
        models.push(model);
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    value.as_u64().filter(|value| *value > 0).or_else(|| {
        value
            .as_f64()
            .filter(|value| *value > 0.0)
            .map(|value| value as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_builtin_models_and_skips_malformed_entries() {
        let payload = json!({
            "builtinModels": [
                {"model_id": "glm-5.2", "model_name": "GLM-5.2", "context_window": 128000, "max_tokens": 8192},
                {"model_id": "openpangu", "context_window": "n/a"},
                {"model_name": ""},
                {"model_id": "glm-5.2", "model_name": "duplicate"}
            ]
        });
        let models = parse_models(&payload);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].display_name.as_deref(), Some("GLM-5.2"));
        assert_eq!(models[0].context_window, Some(128_000));
        assert_eq!(models[0].max_output, Some(8192));
        assert_eq!(models[1].id, "openpangu");
    }

    #[test]
    fn missing_payload_reports_nothing() {
        assert!(parse_models(&json!({"unexpected": true})).is_empty());
    }
}
