//! Persisted model aliases and request-time resolution.
//!
//! Two kinds of names are accepted downstream:
//!
//! * a **native** model id discovered from the provider catalogue;
//! * an **alias** stored in `model_routes`, optionally with several targets.
//!
//! Native ids stay usable. When two providers expose the same native id the
//! request is rejected as ambiguous instead of silently picking one; the caller
//! can disambiguate with `provider-instance::model` or by creating an alias.
//!
//! Aliases never shadow a native id, so resolution order is unambiguous:
//! alias first (it cannot collide), then native.
//!
//! Capability gating: a target is only eligible when it declares the
//! capabilities the request actually needs. `vision` and `reasoning` default to
//! false unless the operator recorded evidence on the route; this gateway never
//! guesses that an upstream model accepts images.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::ModelCatalog;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::{Capabilities, ProviderKey, ProviderRegistry};
use crate::storage::{ModelRoutePatch, ModelRouteRow, NewModelRoute, Storage};

/// Per-target capability overrides. `null`/absent means "inherit from the
/// provider declaration"; `vision`/`reasoning` default to `false`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RouteCapabilities {
    pub streaming: Option<bool>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    pub reasoning: Option<bool>,
}

impl RouteCapabilities {
    pub fn from_json(value: &Value) -> Result<Self, GatewayError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone())
            .map_err(|_| GatewayError::bad_request("route capabilities are invalid"))
    }

    pub(crate) fn effective(&self, declared: &Capabilities) -> EffectiveCapabilities {
        EffectiveCapabilities {
            streaming: self.streaming.unwrap_or(declared.streaming),
            tools: self.tools.unwrap_or(declared.tools),
            vision: self.vision.unwrap_or(false),
            reasoning: self.reasoning.unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
}

/// What the current request needs from a target.
#[derive(Debug, Clone, Copy, Default)]
pub struct RouteNeeds {
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
}

impl RouteNeeds {
    pub(crate) fn missing(&self, caps: &EffectiveCapabilities) -> Option<&'static str> {
        if self.streaming && !caps.streaming {
            return Some("streaming");
        }
        if self.tools && !caps.tools {
            return Some("tool calling");
        }
        if self.vision && !caps.vision {
            return Some("vision input");
        }
        None
    }
}

/// One concrete upstream target after resolution.
#[derive(Debug, Clone)]
pub struct RouteTarget {
    /// `Some` for persisted alias rows, `None` for a native model.
    pub route_id: Option<String>,
    pub provider: ProviderKey,
    pub provider_id: String,
    pub upstream_id: String,
    pub display_name: Option<String>,
    pub capabilities: EffectiveCapabilities,
}

/// Resolution result. A native hit has exactly one target; an alias may have
/// several (already ordered by priority/weight).
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The id echoed back to the downstream client.
    pub exposed_id: String,
    pub is_alias: bool,
    pub targets: Vec<RouteTarget>,
}

#[derive(Clone)]
pub struct ModelRouteService {
    storage: Storage,
}

impl ModelRouteService {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Resolve a downstream model name. The returned target list is ordered:
    /// alias targets by priority desc then weight desc; native hits have one
    /// entry. Every target already passed the capability filter.
    pub async fn resolve(
        &self,
        registry: &ProviderRegistry,
        catalog: &ModelCatalog,
        name: &str,
        needs: RouteNeeds,
    ) -> Result<ResolvedModel, GatewayError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(GatewayError::bad_request("model must not be empty"));
        }
        if is_valid_alias(name) {
            let rows = self.storage.model_routes().list_by_alias(name).await?;
            if !rows.is_empty() {
                return self.resolve_alias(registry, name, &rows, needs).await;
            }
        }
        self.resolve_native(registry, catalog, name, needs)
    }

    async fn resolve_alias(
        &self,
        registry: &ProviderRegistry,
        alias: &str,
        rows: &[ModelRouteRow],
        needs: RouteNeeds,
    ) -> Result<ResolvedModel, GatewayError> {
        let enabled_rows: Vec<&ModelRouteRow> = rows.iter().filter(|row| row.enabled).collect();
        if enabled_rows.is_empty() {
            return Err(GatewayError::model_unavailable(format!(
                "alias `{alias}` exists but every target is disabled"
            )));
        }
        let mut targets = Vec::new();
        let mut reasons: Vec<String> = Vec::new();
        let mut saw_capability_gap = false;
        for row in enabled_rows {
            let Some(info) = registry.info_by_id(&row.provider) else {
                reasons.push(format!("{} is not registered", row.provider));
                continue;
            };
            if info.key().kind != row.kind || info.key().region != row.region {
                reasons.push(format!(
                    "{} is configured for {}-{}, not {}-{}",
                    row.provider,
                    row.kind.as_str(),
                    row.region.as_str(),
                    info.key().kind.as_str(),
                    info.key().region.as_str()
                ));
                continue;
            }
            if !registry.is_enabled(&info.key()) {
                reasons.push(format!("{} is disabled", row.provider));
                continue;
            }
            let Some(caps) = registry.capabilities(&info.key()) else {
                reasons.push(format!("{} has no capability declaration", row.provider));
                continue;
            };
            let override_caps = RouteCapabilities::from_json(&row.capabilities)?;
            let effective = override_caps.effective(&caps);
            if let Some(missing) = needs.missing(&effective) {
                saw_capability_gap = true;
                reasons.push(format!("{} does not support {missing}", row.provider));
                continue;
            }
            targets.push(RouteTarget {
                route_id: Some(row.id.clone()),
                provider: info.key(),
                provider_id: info.id.clone(),
                upstream_id: row.upstream_model.clone(),
                display_name: row.display_name.clone(),
                capabilities: effective,
            });
        }
        if targets.is_empty() {
            let kind = if saw_capability_gap {
                ErrorKind::BadRequest
            } else {
                ErrorKind::ModelUnavailable
            };
            return Err(GatewayError::new(
                kind,
                format!(
                    "no usable target for alias `{alias}`: {}",
                    reasons.join("; ")
                ),
            ));
        }
        Ok(ResolvedModel {
            exposed_id: alias.to_string(),
            is_alias: true,
            targets,
        })
    }

    fn resolve_native(
        &self,
        registry: &ProviderRegistry,
        catalog: &ModelCatalog,
        name: &str,
        needs: RouteNeeds,
    ) -> Result<ResolvedModel, GatewayError> {
        let (explicit_provider, model) = match name.split_once("::") {
            Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
                (Some(provider.trim()), model.trim())
            }
            _ => (None, name),
        };
        let mut matches: Vec<&crate::api::ModelEntry> = catalog
            .entries()
            .iter()
            .filter(|entry| {
                (entry.id == model || entry.upstream_id == model)
                    && registry.is_enabled(&entry.provider)
                    && explicit_provider
                        .map(|wanted| {
                            registry
                                .get(&entry.provider)
                                .map(|info| info.info().id == wanted)
                                .unwrap_or(false)
                        })
                        .unwrap_or(true)
            })
            .collect();
        matches.sort_by_key(|entry| entry.provider.to_string());
        match matches.as_slice() {
            [] => Err(GatewayError::model_unavailable(format!(
                "model `{name}` is not available"
            ))),
            [entry] => {
                let Some(info) = registry.get(&entry.provider) else {
                    return Err(GatewayError::model_unavailable(format!(
                        "model `{name}` is not available"
                    )));
                };
                let info = info.info();
                let caps = registry.capabilities(&entry.provider).unwrap_or_default();
                let effective = RouteCapabilities::default().effective(&caps);
                if let Some(missing) = needs.missing(&effective) {
                    return Err(GatewayError::bad_request(format!(
                        "model `{name}` does not support {missing}"
                    )));
                }
                Ok(ResolvedModel {
                    exposed_id: entry.id.clone(),
                    is_alias: false,
                    targets: vec![RouteTarget {
                        route_id: None,
                        provider: entry.provider,
                        provider_id: info.id.clone(),
                        upstream_id: entry.upstream_id.clone(),
                        display_name: entry.display_name.clone(),
                        capabilities: effective,
                    }],
                })
            }
            entries => {
                let providers: Vec<String> = entries
                    .iter()
                    .map(|entry| {
                        registry
                            .get(&entry.provider)
                            .map(|info| info.info().id.clone())
                            .unwrap_or_else(|| entry.provider.to_string())
                    })
                    .collect();
                Err(GatewayError::bad_request(format!(
                    "model `{model}` is provided by several providers ({}); \
                     send `provider::model` or create an alias",
                    providers.join(", ")
                )))
            }
        }
    }
}

/// Alias grammar: short, no `::` (reserved for native disambiguation), no
/// whitespace.
pub fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 64
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

/// One target to attach to an alias.
#[derive(Debug, Clone)]
pub struct NewTarget {
    pub alias: String,
    pub provider: String,
    pub upstream_model: String,
    pub display_name: Option<String>,
    pub priority: i64,
    pub weight: u32,
    pub capabilities: Value,
}

/// Validate a new target and copy the provider kind/region from the registry,
/// never from the request body.
pub async fn create_target(
    storage: &Storage,
    registry: &ProviderRegistry,
    catalog: &ModelCatalog,
    target: NewTarget,
) -> Result<ModelRouteRow, GatewayError> {
    let NewTarget {
        alias,
        provider,
        upstream_model,
        display_name,
        priority,
        weight,
        capabilities,
    } = target;
    let alias = alias.trim();
    if !is_valid_alias(alias) {
        return Err(GatewayError::bad_request(
            "alias must be 1-64 characters of [A-Za-z0-9._-/] and must not contain `::`",
        ));
    }
    let provider_id = provider.trim();
    let upstream_model = upstream_model.trim();
    if upstream_model.is_empty() || upstream_model.len() > 200 {
        return Err(GatewayError::bad_request(
            "upstream model id must be 1-200 characters",
        ));
    }
    if weight == 0 || weight > 1000 {
        return Err(GatewayError::bad_request(
            "weight must be between 1 and 1000",
        ));
    }
    let info = registry.info_by_id(provider_id).ok_or_else(|| {
        GatewayError::bad_request(format!("provider `{provider_id}` is not enabled"))
    })?;
    if catalog.entries().iter().any(|entry| entry.id == alias) {
        return Err(GatewayError::bad_request(format!(
            "alias `{alias}` conflicts with a native model id; native ids stay directly addressable"
        )));
    }
    RouteCapabilities::from_json(&capabilities)?;
    let routes = storage.model_routes();
    if routes
        .target_exists(alias, &info.id, upstream_model)
        .await?
    {
        return Err(GatewayError::bad_request(
            "this provider/model pair is already a target of the alias",
        ));
    }
    routes
        .insert(&NewModelRoute {
            alias: alias.to_string(),
            provider: info.id.clone(),
            kind: info.key().kind,
            region: info.key().region,
            upstream_model: upstream_model.to_string(),
            display_name,
            priority,
            weight,
            capabilities,
        })
        .await
}

pub async fn patch_target(
    storage: &Storage,
    id: &str,
    patch: &ModelRoutePatch,
) -> Result<ModelRouteRow, GatewayError> {
    if let Some(weight) = patch.weight {
        if weight == 0 || weight > 1000 {
            return Err(GatewayError::bad_request(
                "weight must be between 1 and 1000",
            ));
        }
    }
    if let Some(capabilities) = &patch.capabilities {
        RouteCapabilities::from_json(capabilities)?;
    }
    storage
        .model_routes()
        .update(id, patch)
        .await?
        .ok_or_else(|| GatewayError::bad_request("model route not found"))
}

/// Grouped view for the admin API and `/v1/models`.
#[derive(Debug, Clone, Serialize)]
pub struct AliasView {
    pub alias: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub targets: Vec<TargetView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetView {
    pub id: String,
    pub provider: String,
    pub kind: &'static str,
    pub region: &'static str,
    pub upstream_model: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub weight: u32,
    pub capabilities: Value,
    pub registered: bool,
    pub provider_enabled: bool,
    pub updated_at: DateTime<Utc>,
}

pub async fn list_aliases(
    storage: &Storage,
    registry: &ProviderRegistry,
) -> Result<Vec<AliasView>, GatewayError> {
    let rows = storage.model_routes().list().await?;
    let mut grouped: BTreeMap<String, AliasView> = BTreeMap::new();
    for row in rows {
        let registered = registry.get(&ProviderKey::new(row.kind, row.region));
        let provider_enabled = registered
            .as_ref()
            .map(|info| registry.is_enabled(&info.info().key()))
            .unwrap_or(false);
        let entry = grouped
            .entry(row.alias.clone())
            .or_insert_with(|| AliasView {
                alias: row.alias.clone(),
                display_name: None,
                enabled: false,
                targets: Vec::new(),
            });
        if entry.display_name.is_none() {
            entry.display_name = row.display_name.clone();
        }
        if row.enabled {
            entry.enabled = true;
        }
        entry.targets.push(TargetView {
            id: row.id,
            provider: row.provider,
            kind: row.kind.as_str(),
            region: row.region.as_str(),
            upstream_model: row.upstream_model,
            display_name: row.display_name,
            enabled: row.enabled,
            priority: row.priority,
            weight: row.weight,
            capabilities: row.capabilities,
            registered: registered.is_some(),
            provider_enabled,
            updated_at: row.updated_at,
        });
    }
    Ok(grouped.into_values().collect())
}

/// JSON list item for `GET /v1/models`, mirroring the native entries. Returns
/// `None` when no target is currently usable.
pub fn alias_model_json(view: &AliasView, registry: &ProviderRegistry) -> Option<Value> {
    let mut data = Vec::new();
    let mut tools = false;
    let mut streaming = false;
    let mut vision = false;
    for target in view
        .targets
        .iter()
        .filter(|target| target.enabled && target.provider_enabled)
    {
        let Some(info) = registry.info_by_id(&target.provider) else {
            continue;
        };
        let declared = registry.capabilities(&info.key()).unwrap_or_default();
        let overrides = RouteCapabilities::from_json(&target.capabilities).unwrap_or_default();
        let caps = overrides.effective(&declared);
        tools |= caps.tools;
        streaming |= caps.streaming;
        vision |= caps.vision;
        data.push(json!({
            "provider": target.provider,
            "kind": target.kind,
            "region": target.region,
            "upstream_model": target.upstream_model,
            "priority": target.priority,
            "weight": target.weight,
        }));
    }
    if data.is_empty() {
        return None;
    }
    Some(json!({
        "id": view.alias,
        "object": "model",
        "created": 0,
        "owned_by": "polygate",
        "display_name": view.display_name.clone().unwrap_or_else(|| view.alias.clone()),
        "polygate": {
            "alias": true,
            "enabled": view.enabled,
            "targets": data,
            "capabilities": {
                "chat": true,
                "streaming": streaming,
                "tools": tools,
                "vision": vision,
            }
        }
    }))
}
