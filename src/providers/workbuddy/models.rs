//! WorkBuddy model catalogue parsing.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §6.1. CN and Intl share the
//! `{ models, agents }` shape (usually under `data`). The `agents[name="cli"]`
//! entry lists the models the official CLI may choose from; when it is absent
//! the full `models` array is used.
//!
//! A model is skipped when it is explicitly disabled or, if it has an entry in
//! `models`, when both token limits are missing or non-positive — the reference
//! implementation drops those rather than advertising an unusable model.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::Region;
use crate::error::GatewayError;
use crate::providers::UpstreamModel;

use super::region;

pub fn parse_catalog(region: Region, payload: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let root = match payload.get("data") {
        Some(Value::Object(_)) => payload.get("data").unwrap_or(payload),
        _ => payload,
    };
    let objects = model_objects(root);
    let mut candidates = cli_model_ids(root);
    if candidates.is_empty() {
        candidates = objects.keys().cloned().collect();
    }

    let mut seen = BTreeMap::new();
    let mut models = Vec::new();
    for id in candidates {
        if seen.insert(id.clone(), ()).is_some() {
            continue;
        }
        let object = objects.get(&id);
        if object.is_some_and(|object| is_disabled(object)) {
            continue;
        }
        let mut model = UpstreamModel::new(id);
        if let Some(object) = object {
            model.display_name = object
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            model.context_window = positive(object.get("maxInputTokens"));
            model.max_output = positive(object.get("maxOutputTokens"));
            if model.context_window.is_none() && model.max_output.is_none() {
                continue;
            }
        }
        models.push(model);
    }

    if models.is_empty() {
        return Err(GatewayError::upstream_server(format!(
            "WorkBuddy {} model catalogue contained no usable models",
            region::realm(region).region.as_str()
        )));
    }
    Ok(models)
}

fn model_objects(root: &Value) -> BTreeMap<String, &Value> {
    let mut objects = BTreeMap::new();
    if let Some(items) = root.get("models").and_then(Value::as_array) {
        for item in items {
            if let Some((id, object)) = model_entry(item) {
                objects.insert(id, object);
            }
        }
    }
    objects
}

fn model_entry(item: &Value) -> Option<(String, &Value)> {
    if !item.is_object() {
        return None;
    }
    let id = ["id", "key", "model"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    Some((id.to_string(), item))
}

fn cli_model_ids(root: &Value) -> Vec<String> {
    let Some(agents) = root.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    let Some(cli) = agents.iter().find(|agent| {
        agent
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case("cli"))
    }) else {
        return Vec::new();
    };
    let Some(items) = cli.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(id) => {
                let id = id.trim();
                (!id.is_empty()).then(|| id.to_string())
            }
            Value::Object(object) => ["id", "key", "model"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            _ => None,
        })
        .collect()
}

fn is_disabled(object: &Value) -> bool {
    object
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object
            .get("disabledMultimodal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn positive(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .or_else(|| {
            value
                .and_then(Value::as_f64)
                .filter(|number| *number > 0.0)
                .map(|number| number as u64)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cli_agent_limits_the_catalogue() {
        let payload = json!({
            "data": {
                "models": [
                    {"id": "glm-5.2", "name": "GLM 5.2", "maxInputTokens": 200000, "maxOutputTokens": 32000},
                    {"id": "deepseek-v4-pro", "name": "DeepSeek V4 Pro", "maxInputTokens": 128000, "maxOutputTokens": 16000},
                    {"id": "hidden", "name": "Hidden", "maxInputTokens": 8000, "maxOutputTokens": 2000, "disabled": true}
                ],
                "agents": [{"name": "cli", "models": ["glm-5.2", "hidden"]}]
            }
        });
        let models = parse_catalog(Region::Cn, &payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
        assert_eq!(models[0].context_window, Some(200000));
        assert_eq!(models[0].max_output, Some(32000));
    }

    #[test]
    fn missing_cli_agent_falls_back_to_all_models() {
        let payload = json!({
            "models": [
                {"id": "a", "maxInputTokens": 1000, "maxOutputTokens": 100},
                {"id": "b", "maxInputTokens": 1000, "maxOutputTokens": 100}
            ]
        });
        let models = parse_catalog(Region::Intl, &payload).unwrap();
        assert_eq!(models.len(), 2);
    }

    #[test]
    fn models_without_limits_are_dropped_only_when_described() {
        let payload = json!({
            "models": [
                {"id": "described", "name": "No limits"},
                {"id": "fine", "maxInputTokens": 10, "maxOutputTokens": 1}
            ]
        });
        let models = parse_catalog(Region::Cn, &payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "fine");
    }

    #[test]
    fn an_empty_catalogue_is_a_discovery_failure() {
        let error = parse_catalog(Region::Cn, &json!({"models": []})).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }
}
