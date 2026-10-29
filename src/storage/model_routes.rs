//! Persisted model aliases (`model_routes`).
//!
//! One alias may own several targets. Rows are plain routing data: the provider
//! instance id, its kind/region (copied at write time so a stale row cannot be
//! routed into another realm), the upstream model id and the per-target
//! capability overrides.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use super::{decode_ts, encode_ts, storage_error};

const COLUMNS: &str = "id, alias, provider, provider_kind, region, upstream_model, display_name, \
     enabled, priority, weight, capabilities, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct ModelRouteRow {
    pub id: String,
    pub alias: String,
    pub provider: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub upstream_model: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub priority: i64,
    pub weight: u32,
    pub capabilities: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewModelRoute {
    pub alias: String,
    pub provider: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub upstream_model: String,
    pub display_name: Option<String>,
    pub priority: i64,
    pub weight: u32,
    pub capabilities: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRoutePatch {
    pub display_name: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub priority: Option<i64>,
    pub weight: Option<u32>,
    pub capabilities: Option<Value>,
}

#[derive(Clone)]
pub struct ModelRouteRepo {
    pool: SqlitePool,
}

impl ModelRouteRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<ModelRouteRow>, GatewayError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM model_routes ORDER BY alias, priority DESC, id"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.into_iter().map(map_row).collect()
    }

    pub async fn list_by_alias(&self, alias: &str) -> Result<Vec<ModelRouteRow>, GatewayError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM model_routes WHERE alias = ? \
             ORDER BY priority DESC, weight DESC, id"
        ))
        .bind(alias)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.into_iter().map(map_row).collect()
    }

    pub async fn find(&self, id: &str) -> Result<Option<ModelRouteRow>, GatewayError> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM model_routes WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(map_row).transpose()
    }

    /// Whether this provider/model pair is already a target of the alias.
    pub async fn target_exists(
        &self,
        alias: &str,
        provider: &str,
        upstream_model: &str,
    ) -> Result<bool, GatewayError> {
        let row = sqlx::query(
            "SELECT 1 FROM model_routes WHERE alias = ? AND provider = ? AND upstream_model = ?",
        )
        .bind(alias)
        .bind(provider)
        .bind(upstream_model)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(row.is_some())
    }

    pub async fn insert(&self, new: &NewModelRoute) -> Result<ModelRouteRow, GatewayError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO model_routes (id, alias, provider, provider_kind, region, upstream_model, \
             display_name, enabled, priority, weight, capabilities, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.alias)
        .bind(&new.provider)
        .bind(new.kind.as_str())
        .bind(new.region.as_str())
        .bind(&new.upstream_model)
        .bind(&new.display_name)
        .bind(new.priority)
        .bind(i64::from(new.weight))
        .bind(new.capabilities.to_string())
        .bind(encode_ts(now))
        .bind(encode_ts(now))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        self.find(&id)
            .await?
            .ok_or_else(|| GatewayError::internal("model route vanished after insert"))
    }

    pub async fn update(
        &self,
        id: &str,
        patch: &ModelRoutePatch,
    ) -> Result<Option<ModelRouteRow>, GatewayError> {
        let Some(mut row) = self.find(id).await? else {
            return Ok(None);
        };
        if let Some(display_name) = &patch.display_name {
            row.display_name = display_name.clone();
        }
        if let Some(enabled) = patch.enabled {
            row.enabled = enabled;
        }
        if let Some(priority) = patch.priority {
            row.priority = priority;
        }
        if let Some(weight) = patch.weight {
            if weight == 0 {
                return Err(GatewayError::bad_request("route weight must be at least 1"));
            }
            row.weight = weight;
        }
        if let Some(capabilities) = &patch.capabilities {
            row.capabilities = capabilities.clone();
        }
        let now = Utc::now();
        sqlx::query(
            "UPDATE model_routes SET display_name = ?, enabled = ?, priority = ?, weight = ?, \
             capabilities = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&row.display_name)
        .bind(row.enabled)
        .bind(row.priority)
        .bind(i64::from(row.weight))
        .bind(row.capabilities.to_string())
        .bind(encode_ts(now))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        self.find(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM model_routes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }
}

fn map_row(row: sqlx::sqlite::SqliteRow) -> Result<ModelRouteRow, GatewayError> {
    let kind_text: String = row.try_get("provider_kind").map_err(storage_error)?;
    let region_text: String = row.try_get("region").map_err(storage_error)?;
    let kind = ProviderKind::from_db_str(&kind_text)
        .ok_or_else(|| GatewayError::internal("unknown provider kind in model_routes"))?;
    let region = Region::from_db_str(&region_text)
        .ok_or_else(|| GatewayError::internal("unknown region in model_routes"))?;
    let capabilities: String = row.try_get("capabilities").map_err(storage_error)?;
    Ok(ModelRouteRow {
        id: row.try_get("id").map_err(storage_error)?,
        alias: row.try_get("alias").map_err(storage_error)?,
        provider: row.try_get("provider").map_err(storage_error)?,
        kind,
        region,
        upstream_model: row.try_get("upstream_model").map_err(storage_error)?,
        display_name: row.try_get("display_name").map_err(storage_error)?,
        enabled: row.try_get::<i64, _>("enabled").map_err(storage_error)? == 1,
        priority: row.try_get("priority").map_err(storage_error)?,
        weight: row
            .try_get::<i64, _>("weight")
            .map_err(storage_error)?
            .max(1) as u32,
        capabilities: serde_json::from_str(&capabilities).unwrap_or(Value::Null),
        created_at: decode_ts(
            &row.try_get::<String, _>("created_at")
                .map_err(storage_error)?,
        )?,
        updated_at: decode_ts(
            &row.try_get::<String, _>("updated_at")
                .map_err(storage_error)?,
        )?,
    })
}
