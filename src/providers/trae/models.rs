//! TRAE model discovery.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §4, built from
//! `Coding2API/src/provider/trae/client.py:352-433` (`fetch_models`,
//! `_to_model`, `_context_window`).
//!
//! Model discovery is a **POST** to `/api/ide/v1/get_detail_param` with the
//! directory's own envelope, not an OpenAI `GET /v1/models`. The answer lists
//! every `config_name` a SOLO function may choose from.
//!
//! `context_window_tokens` is a `{dev, max}` pair: `max` is the model's real
//! ceiling and `dev` the conservative tier (a 1M model reports 200k under
//! `dev`). `max` wins whenever it is larger; a `max` at or below `dev` carries
//! no information and is ignored.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::config::Region;
use crate::error::GatewayError;
use crate::providers::UpstreamModel;

use super::region::{self, DEFAULT_FUNCTION};

/// The directory request body, copied field-for-field from the reference.
pub fn models_request_body() -> Value {
    json!({
        "function": DEFAULT_FUNCTION,
        "config_names": null,
        "need_prompt": false,
        "current_config_info": null,
        "poly_prompt": true,
        "mode_type": null,
        "agent_type": null
    })
}

pub fn parse_catalog(region: Region, payload: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let Some(configs) = payload.get("config_info_list").and_then(Value::as_array) else {
        return Err(GatewayError::upstream_server(
            "TRAE models response has no config_info_list",
        ));
    };
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for config in configs {
        let Some(config) = config.as_object() else {
            continue;
        };
        let Some(id) = config
            .get("config_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        let display = config.get("display_config").and_then(Value::as_object);
        let mut model = UpstreamModel::new(id);
        model.display_name = display
            .and_then(|display| display.get("display_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        model.context_window = context_window(config.get("context_window_tokens"));
        models.push(model);
    }
    if models.is_empty() {
        return Err(GatewayError::upstream_server(format!(
            "TRAE {} model catalogue contained no usable models",
            region::realm(region).region.as_str()
        )));
    }
    Ok(models)
}

fn context_window(value: Option<&Value>) -> Option<u64> {
    let window = value?.as_object()?;
    let dev = positive(window.get("dev"));
    let max = positive(window.get("max"));
    match (dev, max) {
        (Some(dev), Some(max)) if max > dev => Some(max),
        (Some(dev), _) => Some(dev),
        (None, max) => max,
    }
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

    #[test]
    fn catalogue_uses_the_max_window() {
        let payload = json!({
            "config_info_list": [
                {
                    "config_name": "glm-5.2",
                    "display_config": {"display_name": "GLM 5.2"},
                    "context_window_tokens": {"dev": 200000, "max": 1000000}
                },
                {
                    "config_name": "conservative",
                    "display_config": {"display_name": ""},
                    "context_window_tokens": {"dev": 128000, "max": 128000}
                },
                {"config_name": "", "display_config": {}},
                {"display_config": {"display_name": "no id"}}
            ]
        });
        let models = parse_catalog(Region::Cn, &payload).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "glm-5.2");
        assert_eq!(models[0].display_name.as_deref(), Some("GLM 5.2"));
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[1].context_window, Some(128_000));
    }

    #[test]
    fn duplicate_ids_are_kept_once() {
        let payload = json!({
            "config_info_list": [
                {"config_name": "m", "context_window_tokens": {"dev": 1000}},
                {"config_name": "m", "context_window_tokens": {"dev": 2000}}
            ]
        });
        let models = parse_catalog(Region::Cn, &payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, Some(1000));
    }

    #[test]
    fn a_missing_list_or_an_empty_list_is_a_failure() {
        assert_eq!(
            parse_catalog(Region::Cn, &json!({})).unwrap_err().kind,
            crate::error::ErrorKind::UpstreamServer
        );
        assert_eq!(
            parse_catalog(Region::Cn, &json!({"config_info_list": []}))
                .unwrap_err()
                .kind,
            crate::error::ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn the_request_body_matches_the_directory_envelope() {
        let body = models_request_body();
        assert_eq!(body["function"], DEFAULT_FUNCTION);
        assert_eq!(body["need_prompt"], false);
        assert_eq!(body["poly_prompt"], true);
    }
}
