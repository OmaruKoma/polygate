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
        );
        model.max_output = integer(item, &["max_output_tokens", "max_tokens"]);
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
                {"name": "no id"}
            ]
        }))
        .expect("catalog");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
        assert_eq!(models[0].context_window, Some(128_000));
        assert_eq!(models[0].max_output, Some(131_072));
        assert_eq!(models[1].id, "qwen3-max");
    }

    #[test]
    fn an_empty_catalogue_is_a_protocol_error() {
        let error = parse_catalog(&json!({"data": []})).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }
}
