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
        // Token limits usually live inside `model_parameters`; the reference
        // extractor reads that object first and the entry itself as fallback.
        let parameters = object
            .get("model_parameters")
            .filter(|value| value.is_object());
        let number = |keys: &[&str]| -> Option<u64> {
            parameters
                .and_then(|params| keys.iter().find_map(|key| positive_u64(params.get(*key))))
                .or_else(|| keys.iter().find_map(|key| positive_u64(object.get(*key))))
        };
        let context_window = number(&[
            "context_window",
            "contextWindow",
            "max_input_tokens",
            "input_length",
            "input_context_window",
        ]);
        let max_output = number(&[
            "max_tokens",
            "maxTokens",
            "max_output_tokens",
            "output_context_window",
        ]);
        let supports_vision = ["supportsImages", "supports_images", "supports_vision"]
            .iter()
            .find_map(|key| {
                parameters
                    .and_then(|params| flag(params.get(*key)))
                    .or_else(|| flag(object.get(*key)))
            })
            .unwrap_or(false);

        let mut model = UpstreamModel::new(id);
        model.display_name = display;
        model.context_window = context_window;
        model.max_output = max_output;
        model.supports_vision = supports_vision;
        models.push(model);
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn flag(value: Option<&Value>) -> Option<bool> {
    let value = value?;
    value
        .as_bool()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
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

/// Extract model-shaped nodes from an agent detail response.
///
/// The agent payload nests models arbitrarily (`model_parameters`, vendor
/// objects, lists); the reference walker visits every node and keeps anything
/// that carries an id plus a model-ish key. Token limits live inside
/// `model_parameters` when present.
pub fn extract_agent_models(payload: &Value) -> Vec<UpstreamModel> {
    let mut out = Vec::new();
    visit_agent_node(payload, &mut out);
    out
}

fn visit_agent_node(node: &Value, out: &mut Vec<UpstreamModel>) {
    match node {
        Value::Object(object) => {
            let id = [
                "model_id",
                "modelId",
                "model_alias",
                "modelAlias",
                "model_name",
                "name",
            ]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty());
            let modelish = object.contains_key("model_id")
                || object.contains_key("modelId")
                || object.contains_key("model_alias")
                || object.contains_key("modelAlias")
                || object.contains_key("model_name")
                || object.contains_key("model_parameters");
            if let (true, Some(id)) = (modelish, id) {
                let parameters = object
                    .get("model_parameters")
                    .filter(|value| value.is_object())
                    .unwrap_or(node);
                let context_window = [
                    "context_window",
                    "contextWindow",
                    "max_input_tokens",
                    "input_length",
                    "input_context_window",
                ]
                .iter()
                .find_map(|key| positive_u64(parameters.get(*key)));
                let max_output = [
                    "max_tokens",
                    "maxTokens",
                    "max_output_tokens",
                    "output_context_window",
                ]
                .iter()
                .find_map(|key| positive_u64(parameters.get(*key)));
                if context_window.is_some() || max_output.is_some() {
                    let mut model = UpstreamModel::new(id);
                    model.context_window = context_window;
                    model.max_output = max_output;
                    out.push(model);
                }
            }
            for nested in object.values() {
                if nested.is_object() || nested.is_array() {
                    visit_agent_node(nested, out);
                }
            }
        }
        Value::Array(items) => {
            for nested in items {
                visit_agent_node(nested, out);
            }
        }
        _ => {}
    }
}

/// Fill context/output/vision gaps in the builtin catalogue from agent details.
pub fn merge_agent_metadata(models: &mut [UpstreamModel], extra: Vec<UpstreamModel>) {
    for model in models.iter_mut() {
        let Some(found) = extra.iter().find(|candidate| candidate.id == model.id) else {
            continue;
        };
        if model.context_window.is_none() {
            model.context_window = found.context_window;
        }
        if model.max_output.is_none() {
            model.max_output = found.max_output;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_builtin_models_and_skips_malformed_entries() {
        let payload = json!({
            "builtinModels": [
                {"model_id": "glm-5.2", "model_name": "GLM-5.2",
                 "model_parameters": {"context_window": 128000, "max_tokens": 8192, "supportsImages": true}},
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
        assert!(models[0].supports_vision);
        assert_eq!(models[1].id, "openpangu");
        assert!(!models[1].supports_vision);
    }

    #[test]
    fn agent_details_fill_token_limits() {
        let payload = json!({
            "agent": {
                "config": {
                    "models": [
                        {"model_id": "glm-5.2",
                         "model_parameters": {"context_window": 200000, "max_tokens": 8192}},
                        {"model_name": "no limits"}
                    ]
                }
            }
        });
        let found = extract_agent_models(&payload);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "glm-5.2");
        assert_eq!(found[0].context_window, Some(200_000));
        assert_eq!(found[0].max_output, Some(8192));

        let mut builtin = vec![UpstreamModel::new("glm-5.2")];
        merge_agent_metadata(&mut builtin, found);
        assert_eq!(builtin[0].context_window, Some(200_000));
    }

    #[test]
    fn missing_payload_reports_nothing() {
        assert!(parse_models(&json!({"unexpected": true})).is_empty());
    }
}
