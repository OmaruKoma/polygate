//! Admin JSON API and the public OAuth callback page.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::accounts::{Account, AuthStatus};
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::providers::{Capability, ProviderKey};
use crate::quota::QuotaSnapshot;

use super::account;
use super::{json_error, no_store, AdminState};

// ---------------------------------------------------------------------------
// Dashboard and read-only views
// ---------------------------------------------------------------------------

pub async fn dashboard(State(state): State<AdminState>) -> Response {
    match dashboard_body(&state).await {
        Ok(body) => json_ok(body),
        Err(error) => json_error(&error),
    }
}

async fn dashboard_body(state: &AdminState) -> Result<Value, GatewayError> {
    let accounts = state.accounts.list().await?;
    let mut enabled = 0usize;
    let mut available = 0usize;
    let mut abnormal = 0usize;
    for account in &accounts {
        let status = state.accounts.credentials().status(&account.id).await?;
        let present = status.as_ref().is_some_and(|status| status.present);
        if account.enabled {
            enabled += 1;
        }
        if account.enabled && account.auth_status.is_dispatchable() && present {
            available += 1;
        }
        if !matches!(account.auth_status, AuthStatus::Unknown | AuthStatus::Valid)
            || account.last_error.is_some()
        {
            abnormal += 1;
        }
    }

    let mut quota_known = 0usize;
    let mut quota_unknown = 0usize;
    for account in &accounts {
        match state
            .storage
            .quota()
            .latest_for_account(&account.id)
            .await?
        {
            Some(snapshot) if snapshot.remaining.is_some() => quota_known += 1,
            _ => quota_unknown += 1,
        }
    }

    let usage = state.storage.usage().summary(None).await?;
    let recent_usage = state.storage.usage().list_recent(5).await?;
    let recent_usage: Vec<Value> = recent_usage
        .into_iter()
        .map(|event| {
            json!({
                "request_id": event.request_id,
                "provider_kind": event.provider_kind,
                "model": event.model,
                "status": event.status,
                "input_tokens": event.input_tokens,
                "output_tokens": event.output_tokens,
                "created_at": event.created_at,
            })
        })
        .collect();

    let since = chrono::Utc::now() - chrono::Duration::hours(24);
    let logs = state.storage.request_logs().summary_since(since).await?;
    let recent_errors: Vec<Value> = state
        .storage
        .request_logs()
        .list_failures(5)
        .await?
        .into_iter()
        .map(|log| {
            json!({
                "request_id": log.request_id,
                "provider_kind": log.provider_kind,
                "model": log.model_alias,
                "api": log.api,
                "http_status": log.http_status,
                "error_category": log.error_category,
                "latency_ms": log.latency_ms,
                "created_at": log.created_at,
            })
        })
        .collect();
    let success_rate = if logs.total > 0 {
        Some((logs.total - logs.failures) as f64 / logs.total as f64)
    } else {
        None
    };

    let mut latest_checkin = None;
    let mut latest_checkin_at = None;
    let mut recent_accounts = Vec::new();
    let mut ordered = accounts.clone();
    ordered.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    for account in ordered.iter().take(5) {
        recent_accounts.push(account::view(state, account).await);
    }
    for account in &accounts {
        if let Some(run) = state
            .checkin
            .history(&account.id, 1)
            .await?
            .into_iter()
            .next()
        {
            if latest_checkin_at
                .map(|at| run.updated_at > at)
                .unwrap_or(true)
            {
                latest_checkin_at = Some(run.updated_at);
                latest_checkin = Some(json!({
                    "account_id": account.id,
                    "account_name": account.name,
                    "business_date": run.business_date,
                    "status": run.status.as_str(),
                    "summary": run.summary,
                    "completed_at": run.completed_at,
                    "updated_at": run.updated_at,
                }));
            }
        }
    }

    Ok(json!({
        "providers": {
            "total": state.registry.list_all().len(),
            "enabled": state.registry.list_enabled().len(),
            "implemented": state.registry.list_all().iter().map(|info| info.kind.as_str()).collect::<std::collections::BTreeSet<_>>(),
        },
        "accounts": {
            "total": accounts.len(),
            "enabled": enabled,
            "available": available,
            "abnormal": abnormal,
        },
        "quota": {
            "known": quota_known,
            "unknown": quota_unknown,
        },
        "requests": {
            "total": usage.requests,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "recent": recent_usage,
        },
        "routing": {
            "success_rate_24h": success_rate,
            "requests_24h": logs.total,
            "failures_24h": logs.failures,
            "avg_latency_ms_24h": logs.avg_latency_ms,
            "recent_errors": recent_errors,
            "strategy": match state.scheduler.config().strategy {
                crate::config::SchedulerStrategy::RoundRobin => "round_robin",
                crate::config::SchedulerStrategy::SmoothWeightedRoundRobin => "smooth_weighted_round_robin",
                crate::config::SchedulerStrategy::LeastConnections => "least_connections",
            },
        },
        "checkin": {
            "latest": latest_checkin,
        },
        "recent_accounts": recent_accounts,
    }))
}

pub async fn providers(State(state): State<AdminState>) -> Response {
    match providers_body(&state).await {
        Ok(body) => json_ok(body),
        Err(error) => json_error(&error),
    }
}

async fn providers_body(state: &AdminState) -> Result<Value, GatewayError> {
    let accounts = state.accounts.list().await?;
    let mut items = Vec::new();
    for info in state.registry.list_all() {
        let mut account_count = 0usize;
        let mut credential_count = 0usize;
        for account in &accounts {
            if account.kind != info.kind || account.region != info.region {
                continue;
            }
            account_count += 1;
            if state
                .accounts
                .credentials()
                .status(&account.id)
                .await?
                .is_some_and(|status| status.present)
            {
                credential_count += 1;
            }
        }
        let capabilities = state.registry.capabilities(&info.key());
        items.push(json!({
            "id": info.id,
            "kind": info.kind.as_str(),
            "region": info.region.as_str(),
            "display_name": info.display_name,
            "verification": info.verification.as_str(),
            "enabled": info.enabled,
            "account_count": account_count,
            "credential_count": credential_count,
            "capabilities": capabilities.map(|caps| json!({
                "authentication": caps.get(Capability::Authentication),
                "model_discovery": caps.get(Capability::ModelDiscovery),
                "quota": caps.get(Capability::Quota),
                "chat": caps.get(Capability::Chat),
                "streaming": caps.get(Capability::Streaming),
                "tool_calling": caps.get(Capability::ToolCalling),
            })),
        }));
    }
    Ok(json!({"items": items}))
}

pub async fn models(State(state): State<AdminState>) -> Response {
    let discovered_at = state.catalog.discovered_at();
    let items: Vec<Value> = state
        .catalog
        .entries()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "provider": entry.provider.to_string(),
                "kind": entry.provider.kind.as_str(),
                "region": entry.provider.region.as_str(),
                "display_name": entry.display_name,
                "context_window": entry.context_window,
                "max_output": entry.max_output,
                "available": state.registry.is_enabled(&entry.provider),
                "discovered_at": discovered_at,
            })
        })
        .collect();
    json_ok(json!({"items": items, "discovered_at": discovered_at}))
}

// ---------------------------------------------------------------------------
// Model aliases / routes
// ---------------------------------------------------------------------------

pub async fn model_routes(State(state): State<AdminState>) -> Response {
    match model_routes_body(&state).await {
        Ok(body) => json_ok(body),
        Err(error) => json_error(&error),
    }
}

async fn model_routes_body(state: &AdminState) -> Result<Value, GatewayError> {
    let aliases = crate::model_service::list_aliases(&state.storage, &state.registry).await?;
    // Grouped by provider (then model id) so the console lists one upstream's
    // models together instead of interleaving whatever order discovery used.
    let mut native: Vec<(String, String, Value)> = state
        .catalog
        .entries()
        .iter()
        .map(|entry| {
            let provider_id = state
                .registry
                .get(&entry.provider)
                .map(|info| info.info().id)
                .unwrap_or_else(|| entry.provider.to_string());
            (
                provider_id.clone(),
                entry.id.clone(),
                json!({
                    "id": entry.id,
                    "provider": provider_id,
                    "kind": entry.provider.kind.as_str(),
                    "region": entry.provider.region.as_str(),
                    "display_name": entry.display_name,
                    "context_window": entry.context_window,
                    "available": state.registry.is_enabled(&entry.provider),
                }),
            )
        })
        .collect();
    native.sort_by(|a, b| {
        a.0.to_ascii_lowercase()
            .cmp(&b.0.to_ascii_lowercase())
            .then_with(|| a.1.cmp(&b.1))
    });
    let native: Vec<Value> = native.into_iter().map(|(_, _, value)| value).collect();
    Ok(json!({
        "aliases": aliases,
        "native": native,
        "discovered_at": state.catalog.discovered_at(),
    }))
}

#[derive(Deserialize)]
pub struct CreateModelRouteRequest {
    pub alias: String,
    pub provider: String,
    pub upstream_model: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

pub async fn create_model_route(
    State(state): State<AdminState>,
    Json(request): Json<CreateModelRouteRequest>,
) -> Response {
    match crate::model_service::create_target(
        &state.storage,
        &state.registry,
        &state.catalog,
        crate::model_service::NewTarget {
            alias: request.alias,
            provider: request.provider,
            upstream_model: request.upstream_model,
            display_name: request.display_name,
            priority: request.priority.unwrap_or(0),
            weight: request.weight.unwrap_or(1),
            capabilities: request.capabilities.unwrap_or(Value::Null),
        },
    )
    .await
    {
        Ok(row) => json_ok(json!({"id": row.id, "alias": row.alias})),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize, Default)]
pub struct PatchModelRouteRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub capabilities: Option<Value>,
}

pub async fn patch_model_route(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(request): Json<PatchModelRouteRequest>,
) -> Response {
    let patch = crate::storage::ModelRoutePatch {
        display_name: request.display_name.map(Some),
        enabled: request.enabled,
        priority: request.priority,
        weight: request.weight,
        capabilities: request.capabilities,
    };
    match crate::model_service::patch_target(&state.storage, &id, &patch).await {
        Ok(row) => json_ok(json!({"id": row.id, "alias": row.alias, "enabled": row.enabled})),
        Err(error) => json_error(&error),
    }
}

pub async fn delete_model_route(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Response {
    match state.storage.model_routes().delete(&id).await {
        Ok(true) => json_ok(json!({"deleted": true})),
        Ok(false) => json_error(&GatewayError::bad_request("model route not found")),
        Err(error) => json_error(&error),
    }
}

pub async fn scheduler(State(state): State<AdminState>) -> Response {
    let accounts = match state.accounts.list().await {
        Ok(accounts) => accounts,
        Err(error) => return json_error(&error),
    };
    let items: Vec<Value> = accounts
        .into_iter()
        .map(|account| {
            let runtime = state.scheduler.runtime(&account.id);
            json!({
                "account_id": account.id,
                "provider": account.provider,
                "kind": account.kind.as_str(),
                "region": account.region.as_str(),
                "enabled": account.enabled,
                "auth_status": account.auth_status.as_str(),
                "weight": account.weight,
                "inflight": runtime.inflight,
                "consecutive_failures": runtime.consecutive_failures,
                "cooling_until": runtime.cooling_until,
            })
        })
        .collect();
    let config = state.scheduler.config();
    json_ok(json!({
        "strategy": match config.strategy {
            crate::config::SchedulerStrategy::RoundRobin => "round_robin",
            crate::config::SchedulerStrategy::SmoothWeightedRoundRobin => "smooth_weighted_round_robin",
            crate::config::SchedulerStrategy::LeastConnections => "least_connections",
        },
        "quota_min_ratio": config.quota_min_ratio,
        "failure_threshold": config.failure_threshold,
        "items": items,
    }))
}

// ---------------------------------------------------------------------------
// Request logs
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct LogsQuery {
    pub limit: Option<i64>,
}

pub async fn request_logs(
    State(state): State<AdminState>,
    Query(query): Query<LogsQuery>,
) -> Response {
    match state
        .storage
        .request_logs()
        .list_recent(query.limit.unwrap_or(100))
        .await
    {
        Ok(logs) => {
            let items: Vec<Value> = logs
                .into_iter()
                .map(|log| {
                    json!({
                        "request_id": log.request_id,
                        "provider": log.provider,
                        "provider_kind": log.provider_kind,
                        "region": log.region,
                        "account_id": log.account_id,
                        "model": log.model_alias,
                        "upstream_model": log.upstream_model,
                        "api": log.api,
                        "stream": log.stream,
                        "http_status": log.http_status,
                        "error_category": log.error_category,
                        "input_tokens": log.input_tokens,
                        "output_tokens": log.output_tokens,
                        "latency_ms": log.latency_ms,
                        "created_at": log.created_at,
                    })
                })
                .collect();
            json_ok(json!({"items": items}))
        }
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize, Default)]
pub struct PurgeLogsRequest {
    /// Delete rows older than this many days.
    pub older_than_days: Option<i64>,
    /// Delete everything. Mutually exclusive with `older_than_days`.
    #[serde(default)]
    pub all: bool,
}

pub async fn purge_request_logs(
    State(state): State<AdminState>,
    Json(request): Json<PurgeLogsRequest>,
) -> Response {
    if request.all {
        return match state.storage.request_logs().delete_all().await {
            Ok(deleted) => json_ok(json!({"deleted": deleted})),
            Err(error) => json_error(&error),
        };
    }
    let Some(days) = request.older_than_days.filter(|days| *days > 0) else {
        return json_error(&GatewayError::bad_request(
            "provide older_than_days > 0 or all = true",
        ));
    };
    let before = chrono::Utc::now() - chrono::Duration::days(days);
    match state.storage.request_logs().purge_before(before).await {
        Ok(deleted) => json_ok(json!({"deleted": deleted, "before": before})),
        Err(error) => json_error(&error),
    }
}

pub async fn checkins(State(state): State<AdminState>) -> Response {
    match checkins_body(&state).await {
        Ok(body) => json_ok(body),
        Err(error) => json_error(&error),
    }
}

async fn checkins_body(state: &AdminState) -> Result<Value, GatewayError> {
    let mut items = Vec::new();
    for account in state.accounts.list().await? {
        let latest = state
            .checkin
            .history(&account.id, 1)
            .await?
            .into_iter()
            .next();
        items.push(json!({
            "account_id": account.id,
            "account_name": account.name,
            "kind": account.kind.as_str(),
            "region": account.region.as_str(),
            "supported": matches!(
                account.kind,
                ProviderKind::Codearts | ProviderKind::Qoder | ProviderKind::Mock
            ),
            "last": latest.map(|run| json!({
                "business_date": run.business_date,
                "status": run.status.as_str(),
                "summary": run.summary,
                "attempt_count": run.attempt_count,
                "completed_at": run.completed_at,
                "error_category": run.error_category.map(|value| value.as_str()),
            })),
        }));
    }
    Ok(json!({"items": items}))
}

pub async fn usage(State(state): State<AdminState>) -> Response {
    match usage_body(&state).await {
        Ok(body) => json_ok(body),
        Err(error) => json_error(&error),
    }
}

async fn usage_body(state: &AdminState) -> Result<Value, GatewayError> {
    let summary = state.storage.usage().summary(None).await?;
    let recent: Vec<Value> = state
        .storage
        .usage()
        .list_recent(20)
        .await?
        .into_iter()
        .map(|event| {
            json!({
                "request_id": event.request_id,
                "provider_kind": event.provider_kind,
                "account_id": event.account_id,
                "model": event.model,
                "input_tokens": event.input_tokens,
                "output_tokens": event.output_tokens,
                "status": event.status,
                "created_at": event.created_at,
            })
        })
        .collect();
    Ok(json!({
        "summary": {
            "requests": summary.requests,
            "input_tokens": summary.input_tokens,
            "output_tokens": summary.output_tokens,
        },
        "items": recent,
    }))
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

pub async fn list_accounts(
    State(state): State<AdminState>,
    Query(query): Query<AccountQuery>,
) -> Response {
    let kind = match query.kind.as_deref() {
        Some(value) => match ProviderKind::from_db_str(value) {
            Some(kind) => Some(kind),
            None => return json_error(&GatewayError::bad_request("unknown provider kind")),
        },
        None => None,
    };
    let region = match query.region.as_deref() {
        Some(value) => match Region::from_db_str(value) {
            Some(region) => Some(region),
            None => return json_error(&GatewayError::bad_request("unknown region")),
        },
        None => None,
    };
    let accounts = match state.accounts.list().await {
        Ok(accounts) => accounts,
        Err(error) => return json_error(&error),
    };
    let mut items = Vec::new();
    for account in accounts {
        if kind.is_some_and(|kind| account.kind != kind)
            || region.is_some_and(|region| account.region != region)
        {
            continue;
        }
        items.push(account::view(&state, &account).await);
    }
    json_ok(json!({"items": items, "count": items.len()}))
}

#[derive(Deserialize)]
pub struct CreateAccountRequest {
    #[serde(default)]
    pub kind: Option<String>,
    pub region: String,
    pub name: String,
}

pub async fn create_account(
    State(state): State<AdminState>,
    Json(request): Json<CreateAccountRequest>,
) -> Response {
    let region = match Region::from_db_str(request.region.trim()) {
        Some(region) => region,
        None => return json_error(&GatewayError::bad_request("region must be cn or intl")),
    };
    if let Some(kind) = request.kind.as_deref() {
        if kind != "codearts" {
            return json_error(&GatewayError::new(
                ErrorKind::Unsupported,
                "only CodeArts accounts can be created in this milestone",
            ));
        }
    }
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return json_error(&GatewayError::bad_request(
            "account name must be 1..=64 characters",
        ));
    }
    let mut create =
        crate::account_service::CreateAccount::new(ProviderKind::Codearts, region, name, None);
    create.provider = format!("codearts-{}", region.as_str());
    match state.accounts.create(create).await {
        Ok(account) => json_ok(account::view(&state, &account).await),
        Err(error) => json_error(&error),
    }
}

pub async fn get_account(State(state): State<AdminState>, Path(id): Path<String>) -> Response {
    match load_account(&state, &id).await {
        Ok(account) => json_ok(account::view(&state, &account).await),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize, Default)]
pub struct PatchAccountRequest {
    pub name: Option<String>,
    pub enabled: Option<bool>,
}

pub async fn patch_account(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(request): Json<PatchAccountRequest>,
) -> Response {
    let account = match load_account(&state, &id).await {
        Ok(account) => account,
        Err(error) => return json_error(&error),
    };
    if let Some(name) = request.name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return json_error(&GatewayError::bad_request(
                "account name must be 1..=64 characters",
            ));
        }
        match state
            .accounts
            .update(
                &account.id,
                crate::account_service::AccountUpdate {
                    name: Some(name.to_string()),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(_) => {}
            Err(error) => return json_error(&error),
        }
    }
    if let Some(enabled) = request.enabled {
        if let Err(error) = state.accounts.set_enabled(&account.id, enabled).await {
            return json_error(&error);
        }
    }
    match load_account(&state, &id).await {
        Ok(account) => json_ok(account::view(&state, &account).await),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    pub confirm: Option<String>,
}

pub async fn delete_account(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Response {
    if query.confirm.as_deref() != Some(id.as_str()) {
        return json_error(&GatewayError::bad_request(
            "account removal requires confirm=<account id>",
        ));
    }
    match state.accounts.delete(&id).await {
        Ok(true) => json_ok(json!({"deleted": true, "account_id": id})),
        Ok(false) => json_error(&GatewayError::new(
            ErrorKind::BadRequest,
            "account not found",
        )),
        Err(error) => json_error(&error),
    }
}

pub async fn local_auth_status(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Response {
    match load_account(&state, &id).await {
        Ok(account) => json_ok(json!({
            "account_id": account.id,
            "auth_status": account.auth_status.as_str(),
            "auth_checked_at": account.auth_checked_at,
            "last_error": account.last_error,
            "credential_present": state
                .accounts
                .credentials()
                .status(&account.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|status| status.present),
        })),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize, Default)]
pub struct AuthProbeRequest {
    #[serde(default)]
    pub probe: bool,
}

pub async fn refresh_auth_status(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(request): Json<AuthProbeRequest>,
) -> Response {
    let account = match load_account(&state, &id).await {
        Ok(account) => account,
        Err(error) => return json_error(&error),
    };
    if !request.probe {
        return json_ok(json!({
            "account_id": account.id,
            "auth_status": account.auth_status.as_str(),
            "auth_checked_at": account.auth_checked_at,
            "last_error": account.last_error,
            "probe": false,
        }));
    }
    let context = match state.accounts.authorization_context(&id).await {
        Ok(context) => context,
        Err(error) => return json_error(&error),
    };
    let Some(credential) = context.credential else {
        return json_error(&GatewayError::bad_request("account has no credential"));
    };
    let key = ProviderKey::new(account.kind, account.region);
    let Some(provider) = state.registry.get(&key) else {
        return json_error(&GatewayError::new(
            ErrorKind::NoCapacity,
            "provider is not registered",
        ));
    };
    // Passing the account context lets an adapter refresh an expiring credential
    // (and persist it) before probing, instead of reporting a dead token.
    let provider_context = crate::providers::ProviderContext {
        account_id: Some(id.clone()),
        credential_revision: context.credential_revision,
        ..Default::default()
    };
    let status = match provider
        .auth_status_with_context(&provider_context, &credential)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            let _ = state.accounts.mark_auth_failure(&id, &error).await;
            return json_error(&error);
        }
    };
    let persisted = if account.auth_status.can_transition_to(status) {
        state
            .accounts
            .record_auth_status(&id, status, None)
            .await
            .is_ok()
    } else {
        false
    };
    json_ok(json!({
        "account_id": id,
        "auth_status": status.as_str(),
        "persisted": persisted,
        "probe": true,
    }))
}

pub async fn get_quota(State(state): State<AdminState>, Path(id): Path<String>) -> Response {
    match state.storage.quota().latest_for_account(&id).await {
        Ok(Some(snapshot)) => json_ok(json!({"quota": snapshot, "unknown": false})),
        Ok(None) => json_ok(json!({"quota": null, "unknown": true})),
        Err(error) => json_error(&error),
    }
}

pub async fn refresh_quota(State(state): State<AdminState>, Path(id): Path<String>) -> Response {
    let account = match load_account(&state, &id).await {
        Ok(account) => account,
        Err(error) => return json_error(&error),
    };
    let context = match state.accounts.authorization_context(&id).await {
        Ok(context) => context,
        Err(error) => return json_error(&error),
    };
    let Some(credential) = context.credential else {
        return json_error(&GatewayError::bad_request("account has no credential"));
    };
    let key = ProviderKey::new(account.kind, account.region);
    let Some(provider) = state.registry.get(&key) else {
        return json_error(&GatewayError::new(
            ErrorKind::NoCapacity,
            "provider is not registered",
        ));
    };
    let provider_context = crate::providers::ProviderContext {
        account_id: Some(account.id.clone()),
        credential_revision: context.credential_revision,
        ..Default::default()
    };
    let snapshot = match provider
        .quota_with_context(&provider_context, &credential)
        .await
    {
        Ok(mut snapshot) => {
            snapshot.account_id = account.id.clone();
            snapshot
        }
        Err(error) => QuotaSnapshot::error(&account.id, error.message),
    };
    if let Err(error) = state.storage.quota().save(&snapshot).await {
        return json_error(&error);
    }
    json_ok(json!({"quota": snapshot, "unknown": snapshot.remaining.is_none()}))
}

pub async fn account_checkins(State(state): State<AdminState>, Path(id): Path<String>) -> Response {
    if let Err(error) = load_account(&state, &id).await {
        return json_error(&error);
    }
    match state.checkin.history(&id, 20).await {
        Ok(runs) => {
            let items: Vec<Value> = runs
                .into_iter()
                .map(|run| {
                    json!({
                        "business_date": run.business_date,
                        "status": run.status.as_str(),
                        "summary": run.summary,
                        "attempt_count": run.attempt_count,
                        "completed_at": run.completed_at,
                        "error_category": run.error_category.map(|value| value.as_str()),
                    })
                })
                .collect();
            json_ok(json!({"items": items}))
        }
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// Check-in run
// ---------------------------------------------------------------------------

/// Run one account's check-in for its current business date.
///
/// This is a mutating, side-effecting action: it is a POST (CSRF + Origin
/// checked) and the service itself refuses to execute the same account/day
/// twice, so a double click cannot claim twice.
pub async fn run_account_checkin(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Response {
    let account = match load_account(&state, &id).await {
        Ok(account) => account,
        Err(error) => return json_error(&error),
    };
    if !account.enabled {
        return json_error(&GatewayError::bad_request("account is disabled"));
    }
    match state.checkin.checkin(&id).await {
        Ok(result) => json_ok(json!({
            "account_id": result.account_id,
            "business_date": result.business_date,
            "status": result.status.as_str(),
            "summary": result.summary,
            "replayed": result.replayed,
            "attempt_count": result.attempt_count,
            "error_category": result.error_category.map(|value| value.as_str()),
            "quota_refresh": result.quota_refresh,
        })),
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// CodeArts authorization
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartAuthRequest {
    pub region: String,
    #[serde(default)]
    pub name: Option<String>,
    /// TRAE only: operator-supplied device identity. Never generated.
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    /// OfficeAce only: log out of the Huawei cloud session first so the
    /// operator can pick the account being added.
    #[serde(default)]
    pub switch_account: Option<bool>,
}

pub async fn start_codearts_auth(
    State(state): State<AdminState>,
    Json(request): Json<StartAuthRequest>,
) -> Response {
    let region = match Region::from_db_str(request.region.trim()) {
        Some(region) => region,
        None => return json_error(&GatewayError::bad_request("region must be cn or intl")),
    };
    match state.start_codearts_flow(region, request.name).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn codearts_flow_status(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.flow_status(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn cancel_codearts_flow(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.cancel_codearts_flow(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// Qoder device authorization
// ---------------------------------------------------------------------------

pub async fn start_qoder_auth(
    State(state): State<AdminState>,
    Json(request): Json<StartAuthRequest>,
) -> Response {
    let region = match Region::from_db_str(request.region.trim()) {
        Some(region) => region,
        None => return json_error(&GatewayError::bad_request("region must be cn or intl")),
    };
    match state.start_qoder_flow(region, request.name).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn qoder_flow_status(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.qoder_flow_status(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn cancel_qoder_flow(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.cancel_qoder_flow(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// OfficeAce authorization
// ---------------------------------------------------------------------------

pub async fn start_officeace_auth(
    State(state): State<AdminState>,
    Json(request): Json<StartAuthRequest>,
) -> Response {
    if request.region.trim() != "cn" {
        return json_error(&GatewayError::bad_request(
            "OfficeAce only has a CN deployment; use region = cn",
        ));
    }
    match state
        .start_officeace_flow(request.name, request.switch_account.unwrap_or(false))
        .await
    {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn officeace_flow_status(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.officeace_flow_status(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn cancel_officeace_flow(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.cancel_officeace_flow(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// WorkBuddy device authorization
// ---------------------------------------------------------------------------

pub async fn start_workbuddy_auth(
    State(state): State<AdminState>,
    Json(request): Json<StartAuthRequest>,
) -> Response {
    let region = match Region::from_db_str(request.region.trim()) {
        Some(region) => region,
        None => return json_error(&GatewayError::bad_request("region must be cn or intl")),
    };
    match state.start_workbuddy_flow(region, request.name).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn workbuddy_flow_status(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.workbuddy_flow_status(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn cancel_workbuddy_flow(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.cancel_workbuddy_flow(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

// ---------------------------------------------------------------------------
// TRAE authorization
// ---------------------------------------------------------------------------

pub async fn start_trae_auth(
    State(state): State<AdminState>,
    Json(request): Json<StartAuthRequest>,
) -> Response {
    let region = match Region::from_db_str(request.region.trim()) {
        Some(region) => region,
        None => return json_error(&GatewayError::bad_request("region must be cn or intl")),
    };
    match state
        .start_trae_flow(region, request.name, request.machine_id, request.device_id)
        .await
    {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn trae_flow_status(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.trae_flow_status(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize)]
pub struct CompleteTraeRequest {
    /// The full callback URL copied from the browser address bar.
    pub url: String,
}

pub async fn cancel_active_auth(State(state): State<AdminState>) -> Response {
    match state.cancel_active_flow().await {
        Ok(Some(view)) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Ok(None) => json_ok(Value::Null),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize)]
pub struct CompleteCodeartsRequest {
    /// The full callback URL copied from the browser address bar, or the code.
    pub url: String,
}

pub async fn complete_codearts_auth(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
    Json(request): Json<CompleteCodeartsRequest>,
) -> Response {
    match state
        .complete_codearts_flow(&flow_id, request.url.trim())
        .await
    {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn complete_trae_auth(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
    Json(request): Json<CompleteTraeRequest>,
) -> Response {
    match state.complete_trae_flow(&flow_id, request.url.trim()).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

pub async fn cancel_trae_flow(
    State(state): State<AdminState>,
    Path(flow_id): Path<String>,
) -> Response {
    match state.cancel_trae_flow(&flow_id).await {
        Ok(view) => json_ok(serde_json::to_value(view).unwrap_or(Value::Null)),
        Err(error) => json_error(&error),
    }
}

#[derive(Deserialize, Default)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
    /// TRAE puts the refresh token on the callback URL. The gateway never
    /// completes the flow from this unauthenticated GET; the page only tells
    /// the operator to paste the address back into the panel.
    #[serde(default, rename = "refreshToken")]
    pub refresh_token: Option<String>,
}

pub async fn oauth_callback(
    State(state): State<AdminState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let _ = query.error_description.as_ref();
    if query
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        return trae_paste_page();
    }
    if query.error.is_some() {
        // Do not let an unauthenticated hit cancel a live flow; it will expire
        // or the administrator can cancel it explicitly.
        return callback_page(false, None, None);
    }
    let Some(code) = query.code.filter(|code| !code.trim().is_empty()) else {
        return callback_page(false, None, None);
    };
    match state.complete_active_flow(&code).await {
        Ok(flow) => callback_page(true, Some(&flow.flow_id), flow.account_id.as_deref()),
        Err(_) => callback_page(false, None, None),
    }
}

/// TRAE's callback carries token material in the URL. The browser page never
/// echoes it and never finalizes the flow; it asks the operator to paste the
/// address back into the authenticated panel.
fn trae_paste_page() -> Response {
    let html = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>请返回 PolyGate 完成授权</title>
<link rel="stylesheet" href="/admin/static/app.css">
</head>
<body class="oauth-page" data-success="false">
<main class="oauth-card">
<div class="oauth-dot ok"></div>
<h1>授权信息已返回</h1>
<p>请复制本页地址栏中的完整链接，粘贴回 PolyGate 的 TRAE 授权窗口完成绑定。链接包含凭据，请勿分享。</p>
<button type="button" class="btn primary" id="oauth-close">关闭窗口</button>
</main>
<script src="/admin/static/oauth-callback.js"></script>
</body></html>"#;
    let mut response = (StatusCode::OK, Html(html)).into_response();
    no_store(&mut response);
    response
}

fn callback_page(success: bool, flow_id: Option<&str>, account_id: Option<&str>) -> Response {
    let title = if success {
        "授权成功"
    } else {
        "授权失败"
    };
    let message = if success {
        "CodeArts 账号已连接，正在返回 PolyGate。"
    } else {
        "CodeArts 授权未完成，请返回 PolyGate 重试。"
    };
    let html = format!(
        r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} · PolyGate</title>
<link rel="stylesheet" href="/admin/static/app.css">
</head>
<body class="oauth-page" data-success="{success}" data-flow-id="{flow}" data-account-id="{account}">
<main class="oauth-card">
<div class="oauth-dot {class}"></div>
<h1>{title}</h1>
<p>{message}</p>
<button type="button" class="btn primary" id="oauth-close">关闭窗口</button>
</main>
<script src="/admin/static/oauth-callback.js"></script>
</body></html>"#,
        success = success,
        class = if success { "ok" } else { "fail" },
        flow = flow_id.unwrap_or_default(),
        account = account_id.unwrap_or_default(),
    );
    let mut response = (StatusCode::OK, Html(html)).into_response();
    no_store(&mut response);
    response
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct AccountQuery {
    pub kind: Option<String>,
    pub region: Option<String>,
}

pub fn json_ok(value: Value) -> Response {
    let mut response = Json(value).into_response();
    no_store(&mut response);
    response
}

async fn load_account(state: &AdminState, id: &str) -> Result<Account, GatewayError> {
    state
        .accounts
        .get(id)
        .await?
        .ok_or_else(|| GatewayError::bad_request("account not found"))
}

// ---------------------------------------------------------------------------
// Loomy session import
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoomySessionRequest {
    /// The 32-character login session copied from the client.
    pub session: String,
    #[serde(default)]
    pub userid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Unix seconds; defaults to 14 days from now (the documented lifetime).
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// Import a Loomy login session and probe it before storing anything.
///
/// The probe is `GET /models`, which is read-only: a rejected session is never
/// written to the database, and a valid one is stored only through
/// `CredentialService` (SecretBox + revision CAS).
pub async fn import_loomy_session(
    State(state): State<AdminState>,
    Json(request): Json<LoomySessionRequest>,
) -> Response {
    let session = request.session.trim().to_string();
    if session.len() < 16 || session.len() > 512 || session.chars().any(char::is_whitespace) {
        return json_error(&GatewayError::bad_request(
            "paste the Loomy session value only (one token, no spaces)",
        ));
    }
    let key = ProviderKey::new(ProviderKind::Loomy, Region::Cn);
    let Some(provider) = state.registry.get(&key) else {
        return json_error(&GatewayError::new(
            ErrorKind::NoCapacity,
            "provider loomy is not enabled in the configuration; add a [[providers]] block with kind = \"loomy\" and restart",
        ));
    };
    let candidate = crate::accounts::Credential::from_value(json!({
        "session": session,
        "userid": request.userid.clone().unwrap_or_default(),
    }));
    let models = match provider.list_models(&candidate).await {
        Ok(models) if !models.is_empty() => models,
        Ok(_) => {
            return json_error(&GatewayError::new(
                ErrorKind::NoCapacity,
                "Loomy returned an empty model catalogue for this session",
            ));
        }
        Err(error) => return json_error(&error),
    };
    let expires_at = request.expires_at.unwrap_or_else(|| {
        chrono::Utc::now().timestamp() + crate::providers::loomy::region::SESSION_TTL_SECONDS
    });
    match account::persist_loomy_session(
        &state,
        session,
        request.userid,
        expires_at,
        request.name.as_deref(),
    )
    .await
    {
        Ok(account) => json_ok(json!({
            "account_id": account.id,
            "name": account.name,
            "external_id": account.external_id,
            "expires_at": expires_at,
            "models": models.len(),
            // The model catalogue is discovered at startup; a new account only
            // contributes models after a restart.
            "restart_required": true,
        })),
        Err(error) => json_error(&error),
    }
}
