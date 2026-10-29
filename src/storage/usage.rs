//! Append-only usage events (per request; tokens are summed separately).

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use crate::error::GatewayError;

use super::{decode_ts, encode_ts, limit as clamp_limit, storage_error};

#[derive(Debug, Clone)]
pub struct NewUsageEvent {
    pub request_id: String,
    pub provider_kind: String,
    pub account_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub request_id: String,
    pub provider_kind: String,
    pub account_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageSummary {
    /// Number of recorded requests (not sessions, not tokens).
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Clone)]
pub struct UsageRepo {
    pool: SqlitePool,
}

impl UsageRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// `request_id` is unique; a retry must use a new request id.
    pub async fn record(&self, event: &NewUsageEvent) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO usage_events (request_id, provider_kind, account_id, model, \
             input_tokens, output_tokens, status, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.request_id)
        .bind(&event.provider_kind)
        .bind(&event.account_id)
        .bind(&event.model)
        .bind(event.input_tokens)
        .bind(event.output_tokens)
        .bind(&event.status)
        .bind(encode_ts(Utc::now()))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    /// Request count and token totals; pass an account id to scope it.
    pub async fn summary(&self, account_id: Option<&str>) -> Result<UsageSummary, GatewayError> {
        let (sql, scope) = match account_id {
            Some(_) => (
                "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), \
                 COALESCE(SUM(output_tokens), 0) FROM usage_events WHERE account_id = ?",
                true,
            ),
            None => (
                "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), \
                 COALESCE(SUM(output_tokens), 0) FROM usage_events",
                false,
            ),
        };
        let mut query = sqlx::query(sql);
        if scope {
            query = query.bind(account_id);
        }
        let row = query.fetch_one(&self.pool).await.map_err(storage_error)?;
        Ok(UsageSummary {
            requests: row.try_get(0)?,
            input_tokens: row.try_get(1)?,
            output_tokens: row.try_get(2)?,
        })
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<UsageEvent>, GatewayError> {
        let rows = sqlx::query(
            "SELECT request_id, provider_kind, account_id, model, input_tokens, output_tokens, \
             status, created_at FROM usage_events ORDER BY id DESC LIMIT ?",
        )
        .bind(clamp_limit(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter()
            .map(|row| {
                let created_raw: String = row.try_get("created_at")?;
                Ok(UsageEvent {
                    request_id: row.try_get("request_id")?,
                    provider_kind: row.try_get("provider_kind")?,
                    account_id: row.try_get("account_id")?,
                    model: row.try_get("model")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    status: row.try_get("status")?,
                    created_at: decode_ts(&created_raw)?,
                })
            })
            .collect()
    }
}
