//! Request log storage.
//!
//! Only routing metadata and counters are stored: provider instance, account
//! id, public alias, upstream model id, latency, status, error category and
//! token counts. Prompt text, tool arguments and credentials never reach this
//! table.

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use crate::error::GatewayError;

use super::{decode_ts, encode_ts, limit as clamp_limit, storage_error};

#[derive(Debug, Clone)]
pub struct NewRequestLog {
    pub request_id: String,
    pub provider: Option<String>,
    pub provider_kind: String,
    pub region: Option<String>,
    pub account_id: Option<String>,
    pub model_alias: Option<String>,
    pub upstream_model: Option<String>,
    pub api: String,
    pub stream: bool,
    pub http_status: u16,
    pub error_category: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub latency_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RequestLog {
    pub request_id: String,
    pub provider: Option<String>,
    pub provider_kind: String,
    pub region: Option<String>,
    pub account_id: Option<String>,
    pub model_alias: Option<String>,
    pub upstream_model: Option<String>,
    pub api: String,
    pub stream: bool,
    pub http_status: u16,
    pub error_category: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub latency_ms: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LogSummary {
    pub total: i64,
    pub failures: i64,
    pub avg_latency_ms: f64,
}

#[derive(Clone)]
pub struct RequestLogRepo {
    pool: SqlitePool,
}

impl RequestLogRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn record(&self, log: &NewRequestLog) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO request_logs (request_id, provider, provider_kind, region, account_id, \
             model_alias, upstream_model, api, stream, http_status, error_category, \
             input_tokens, output_tokens, latency_ms, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&log.request_id)
        .bind(&log.provider)
        .bind(&log.provider_kind)
        .bind(&log.region)
        .bind(&log.account_id)
        .bind(&log.model_alias)
        .bind(&log.upstream_model)
        .bind(&log.api)
        .bind(i64::from(log.stream))
        .bind(i64::from(log.http_status))
        .bind(&log.error_category)
        .bind(log.input_tokens)
        .bind(log.output_tokens)
        .bind(log.latency_ms)
        .bind(encode_ts(Utc::now()))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<RequestLog>, GatewayError> {
        let rows = sqlx::query(
            "SELECT request_id, provider, provider_kind, region, account_id, model_alias, \
             upstream_model, api, stream, http_status, error_category, input_tokens, \
             output_tokens, latency_ms, created_at FROM request_logs \
             ORDER BY id DESC LIMIT ?",
        )
        .bind(clamp_limit(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(map_row).collect()
    }

    /// Delete rows older than `before`; returns the number removed.
    pub async fn purge_before(&self, before: DateTime<Utc>) -> Result<u64, GatewayError> {
        let result = sqlx::query("DELETE FROM request_logs WHERE created_at < ?")
            .bind(encode_ts(before))
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected())
    }

    /// Aggregate counts since `since`, for the dashboard. Unknown/unlogged
    /// periods are not invented: no rows means `total == 0`.
    pub async fn summary_since(&self, since: DateTime<Utc>) -> Result<LogSummary, GatewayError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total, \
             COALESCE(SUM(CASE WHEN http_status >= 400 THEN 1 ELSE 0 END), 0) AS failures, \
             COALESCE(AVG(latency_ms), 0.0) AS avg_latency \
             FROM request_logs WHERE created_at >= ?",
        )
        .bind(encode_ts(since))
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(LogSummary {
            total: row.try_get::<i64, _>("total").map_err(storage_error)?,
            failures: row.try_get::<i64, _>("failures").map_err(storage_error)?,
            avg_latency_ms: row
                .try_get::<f64, _>("avg_latency")
                .map_err(storage_error)?,
        })
    }

    /// Most recent failed requests (status >= 400) for the dashboard.
    pub async fn list_failures(&self, limit: i64) -> Result<Vec<RequestLog>, GatewayError> {
        let rows = sqlx::query(
            "SELECT request_id, provider, provider_kind, region, account_id, model_alias, \
             upstream_model, api, stream, http_status, error_category, input_tokens, \
             output_tokens, latency_ms, created_at FROM request_logs \
             WHERE http_status >= 400 ORDER BY id DESC LIMIT ?",
        )
        .bind(clamp_limit(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(map_row).collect()
    }

    pub async fn delete_all(&self) -> Result<u64, GatewayError> {
        let result = sqlx::query("DELETE FROM request_logs")
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected())
    }
}

fn map_row(row: &sqlx::sqlite::SqliteRow) -> Result<RequestLog, GatewayError> {
    Ok(RequestLog {
        request_id: row.try_get("request_id").map_err(storage_error)?,
        provider: row.try_get("provider").map_err(storage_error)?,
        provider_kind: row.try_get("provider_kind").map_err(storage_error)?,
        region: row.try_get("region").map_err(storage_error)?,
        account_id: row.try_get("account_id").map_err(storage_error)?,
        model_alias: row.try_get("model_alias").map_err(storage_error)?,
        upstream_model: row.try_get("upstream_model").map_err(storage_error)?,
        api: row.try_get("api").map_err(storage_error)?,
        stream: row.try_get::<i64, _>("stream").map_err(storage_error)? == 1,
        http_status: row
            .try_get::<i64, _>("http_status")
            .map_err(storage_error)? as u16,
        error_category: row.try_get("error_category").map_err(storage_error)?,
        input_tokens: row.try_get("input_tokens").map_err(storage_error)?,
        output_tokens: row.try_get("output_tokens").map_err(storage_error)?,
        latency_ms: row.try_get("latency_ms").map_err(storage_error)?,
        created_at: decode_ts(
            &row.try_get::<String, _>("created_at")
                .map_err(storage_error)?,
        )?,
    })
}
