//! Non-sensitive operator settings.
//!
//! The master key, credentials and gateway tokens never belong here; keys that
//! look like secrets are rejected instead of being silently stored.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::GatewayError;

use super::{encode_ts, storage_error};

const SENSITIVE_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "master",
    "credential",
    "cookie",
    "authorization",
    "private_key",
];

#[derive(Clone)]
pub struct SettingsRepo {
    pool: SqlitePool,
}

impl SettingsRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, GatewayError> {
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), GatewayError> {
        if key.trim().is_empty() || key.len() > 128 {
            return Err(GatewayError::bad_request(
                "settings key must be 1..=128 bytes",
            ));
        }
        if is_sensitive_key(key) {
            return Err(GatewayError::bad_request(
                "sensitive settings keys cannot be persisted",
            ));
        }
        if value.len() > 8192 {
            return Err(GatewayError::bad_request("settings value is too large"));
        }
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
             updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(encode_ts(Utc::now()))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn list(&self) -> Result<Vec<(String, String)>, GatewayError> {
        let rows =
            sqlx::query_as::<_, (String, String)>("SELECT key, value FROM settings ORDER BY key")
                .fetch_all(&self.pool)
                .await
                .map_err(storage_error)?;
        Ok(rows)
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_FRAGMENTS.iter().any(|f| lower.contains(f))
}
