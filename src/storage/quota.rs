//! Quota snapshot history.
//!
//! Unknown balances are stored as `NULL` in `used`/`limit_value`, never as 0.
//! Group queries return the latest row per account; callers must not sum a
//! shared pool (that logic lives in `crate::quota`).

use sqlx::{Row, SqlitePool};

use crate::error::GatewayError;
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

use super::{decode_ts, encode_ts, limit as clamp_limit, storage_error};

const COLUMNS: &str = "id, account_id, quota_group_id, model, quota_unit, used, limit_value, \
     status, error, refreshed_at, raw";

#[derive(Clone)]
pub struct QuotaRepo {
    pool: SqlitePool,
}

impl QuotaRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn save(&self, snapshot: &QuotaSnapshot) -> Result<i64, GatewayError> {
        let used = match (snapshot.total, snapshot.remaining) {
            (Some(total), Some(remaining)) => Some(total - remaining),
            _ => None,
        };
        let result = sqlx::query(
            "INSERT INTO quota_snapshots (account_id, quota_group_id, model, quota_unit, used, \
             limit_value, status, error, refreshed_at, raw) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&snapshot.account_id)
        .bind(&snapshot.group_id)
        .bind(&snapshot.model)
        .bind(snapshot.unit.as_str())
        .bind(used)
        .bind(snapshot.total)
        .bind(snapshot.status.as_str())
        .bind(&snapshot.error)
        .bind(encode_ts(snapshot.refreshed_at))
        .bind(snapshot.raw.as_ref().map(ToString::to_string))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.last_insert_rowid())
    }

    pub async fn latest_for_account(
        &self,
        account_id: &str,
    ) -> Result<Option<QuotaSnapshot>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM quota_snapshots WHERE account_id = ? \
             ORDER BY refreshed_at DESC, id DESC LIMIT 1"
        );
        let row = sqlx::query(&query)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_snapshot(&row)).transpose()
    }

    /// Latest snapshot per account in a group (shared pools are not summed).
    pub async fn latest_for_group(
        &self,
        quota_group_id: &str,
    ) -> Result<Vec<QuotaSnapshot>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM quota_snapshots WHERE id IN (\
                 SELECT MAX(id) FROM quota_snapshots WHERE quota_group_id = ? GROUP BY account_id\
             ) ORDER BY account_id"
        );
        let rows = sqlx::query(&query)
            .bind(quota_group_id)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_snapshot).collect()
    }

    pub async fn history_for_account(
        &self,
        account_id: &str,
        limit: i64,
    ) -> Result<Vec<QuotaSnapshot>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM quota_snapshots WHERE account_id = ? \
             ORDER BY refreshed_at DESC, id DESC LIMIT ?"
        );
        let rows = sqlx::query(&query)
            .bind(account_id)
            .bind(clamp_limit(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_snapshot).collect()
    }
}

fn map_snapshot(row: &sqlx::sqlite::SqliteRow) -> Result<QuotaSnapshot, GatewayError> {
    let unit_raw: String = row.try_get("quota_unit")?;
    let status_raw: String = row.try_get("status")?;
    let refreshed_raw: String = row.try_get("refreshed_at")?;
    let raw_json: Option<String> = row.try_get("raw")?;
    let limit_value: Option<f64> = row.try_get("limit_value")?;
    let used: Option<f64> = row.try_get("used")?;
    let remaining = match (limit_value, used) {
        (Some(total), Some(used)) => Some(total - used),
        _ => None,
    };

    Ok(QuotaSnapshot {
        account_id: row.try_get("account_id")?,
        group_id: row.try_get("quota_group_id")?,
        model: row.try_get("model")?,
        unit: QuotaUnit::from_db_str(&unit_raw),
        total: limit_value,
        remaining,
        status: QuotaStatus::from_db_str(&status_raw),
        error: row.try_get("error")?,
        refreshed_at: decode_ts(&refreshed_raw)?,
        raw: raw_json.and_then(|raw| serde_json::from_str(&raw).ok()),
    })
}
