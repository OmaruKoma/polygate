//! `GET /v2/models` → gateway model list.
//!
//! The catalogue is standard OpenAI-shaped (`data[]`), and the upstream lists
//! models this account may not actually use (they answer `81004`/`81009`). The
//! reference implementation deliberately does **not** probe them: probing is a
//! real inference and spends points. This adapter keeps the same policy and
//! lets real requests mark a model unavailable.

use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::providers::UpstreamModel;

pub fn parse_catalog(value: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let items = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GatewayError::upstream_server("OfficeAce models response has no data list")
        })?;
    let mut models = Vec::with_capacity(items.len());
    for item in items {
        let id = ["id", "model", "model_name"]
            .iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let Some(id) = id else { continue };
        let mut model = UpstreamModel::new(id);
        if let Some(name) = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            model.display_name = Some(name.to_string());
        }
        model.context_window = integer(
            item,
            &["context_length", "max_context_tokens", "max_input_tokens"],
        )
        .or_else(|| known_context(id));
        model.max_output = integer(item, &["max_output_tokens", "max_tokens"]);
        model.supports_vision = known_vision(id);
        models.push(model);
    }
    if models.is_empty() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "OfficeAce models response has no usable entries",
        ));
    }
    Ok(models)
}

/// Curated context lengths from `officeace2api`'s `KNOWN_META` table
/// (MIT, `Q-shuang-dot/officeace2api`, `src/models.mjs`). `/v2/models` does not
/// publish these, so they are used only as a fallback declaration and never
/// override an upstream value.
fn known_context(id: &str) -> Option<u64> {
    match id {
        "deepseek-v4.1-flash"
        | "deepseek-v4-flash"
        | "deepseek-v4-pro"
        | "deepseek-v4-flash-0731" => Some(1_048_576),
        "glm-5.2" | "glm-5.3" | "glm-5.1" => Some(202_752),
        "kimi-k2.6" | "Kimi-K2.6" => Some(262_144),
        "qwen3-32b" | "qwen3-30b-a3b" | "openpangu-2.0-pro" | "openpangu-2.0-flash" => {
            Some(131_072)
        }
        _ => None,
    }
}

/// Same table marks `deepseek-v4.1-flash` as vision-capable; the live red/blue
/// test agreed.
fn known_vision(id: &str) -> bool {
    id == "deepseek-v4.1-flash"
}

fn integer(item: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        item.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entries_keep_their_ids_and_metadata() {
        let models = parse_catalog(&json!({
            "data": [
                {"id": "glm-5.2", "name": "GLM 5.2", "context_length": 128000, "max_output_tokens": 131072},
                {"id": "qwen3-max"},
                {"id": "kimi-k2.6"},
                {"name": "no id"}
            ]
        }))
        .expect("catalog");
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
        assert_eq!(models[0].context_window, Some(128_000));
        assert_eq!(models[0].max_output, Some(131_072));
        assert_eq!(models[1].id, "qwen3-max");
        assert_eq!(models[2].id, "kimi-k2.6");
        assert_eq!(models[2].context_window, Some(262_144));
    }

    #[test]
    fn an_empty_catalogue_is_a_protocol_error() {
        let error = parse_catalog(&json!({"data": []})).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }
}
