//! Admin users, sessions and pending authorization flows.
//!
//! Passwords/recovery codes arrive already hashed; session cookies arrive as
//! SHA-256 hashes. Pending flow secrets are ciphertext produced by SecretBox.

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use crate::error::GatewayError;

use super::{decode_ts, encode_ts, storage_error};

#[derive(Debug, Clone)]
pub struct AdminUserRow {
    pub username: String,
    pub password_hash: String,
    pub recovery_hash: String,
}

#[derive(Debug, Clone)]
pub struct AdminSessionRow {
    pub id: String,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AuthFlowRow {
    pub id: String,
    pub kind: String,
    pub region: String,
    pub status: String,
    pub authorize_url: String,
    pub redirect_uri: String,
    pub ticket_id: String,
    pub secrets_encrypted: String,
    pub requested_name: Option<String>,
    pub account_id: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

const FLOW_COLUMNS: &str = "id, kind, region, status, authorize_url, redirect_uri, ticket_id, \
     secrets_encrypted, requested_name, account_id, error, created_at, expires_at, completed_at";

#[derive(Clone)]
pub struct AdminRepo {
    pool: SqlitePool,
}

impl AdminRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn user(&self) -> Result<Option<AdminUserRow>, GatewayError> {
        let row = sqlx::query(
            "SELECT username, password_hash, recovery_hash FROM admin_users WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(|row| {
            Ok(AdminUserRow {
                username: row.try_get("username")?,
                password_hash: row.try_get("password_hash")?,
                recovery_hash: row.try_get("recovery_hash")?,
            })
        })
        .transpose()
    }

    pub async fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        recovery_hash: &str,
    ) -> Result<bool, GatewayError> {
        let now = encode_ts(Utc::now());
        let result = sqlx::query(
            "INSERT INTO admin_users (singleton, username, password_hash, recovery_hash, \
             created_at, updated_at) VALUES (1, ?, ?, ?, ?, ?) \
             ON CONFLICT(singleton) DO NOTHING",
        )
        .bind(username)
        .bind(password_hash)
        .bind(recovery_hash)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn set_credentials(
        &self,
        password_hash: &str,
        recovery_hash: &str,
    ) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE admin_users SET password_hash = ?, recovery_hash = ?, updated_at = ? \
             WHERE singleton = 1",
        )
        .bind(password_hash)
        .bind(recovery_hash)
        .bind(encode_ts(Utc::now()))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn create_session(
        &self,
        id: &str,
        token_hash: &str,
        csrf_token: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), GatewayError> {
        let now = encode_ts(Utc::now());
        sqlx::query(
            "INSERT INTO admin_sessions (id, token_hash, csrf_token, created_at, last_seen_at, \
             expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(token_hash)
        .bind(csrf_token)
        .bind(&now)
        .bind(&now)
        .bind(encode_ts(expires_at))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn session_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<AdminSessionRow>, GatewayError> {
        let row = sqlx::query(
            "SELECT id, csrf_token, expires_at, revoked_at FROM admin_sessions \
             WHERE token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(|row| {
            let expires_raw: String = row.try_get("expires_at")?;
            let revoked_raw: Option<String> = row.try_get("revoked_at")?;
            Ok(AdminSessionRow {
                id: row.try_get("id")?,
                csrf_token: row.try_get("csrf_token")?,
                expires_at: decode_ts(&expires_raw)?,
                revoked_at: revoked_raw.as_deref().map(decode_ts).transpose()?,
            })
        })
        .transpose()
    }

    pub async fn touch_session(&self, id: &str) -> Result<(), GatewayError> {
        sqlx::query("UPDATE admin_sessions SET last_seen_at = ? WHERE id = ?")
            .bind(encode_ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    pub async fn revoke_session(&self, id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE admin_sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(encode_ts(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn revoke_all_sessions(&self) -> Result<(), GatewayError> {
        sqlx::query("UPDATE admin_sessions SET revoked_at = ? WHERE revoked_at IS NULL")
            .bind(encode_ts(Utc::now()))
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    pub async fn prune_sessions(&self, now: DateTime<Utc>) -> Result<(), GatewayError> {
        sqlx::query("DELETE FROM admin_sessions WHERE expires_at <= ?")
            .bind(encode_ts(now))
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    pub async fn create_flow(&self, flow: &AuthFlowRow) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO admin_auth_flows (id, kind, region, status, authorize_url, redirect_uri, \
             ticket_id, secrets_encrypted, requested_name, account_id, error, created_at, \
             expires_at, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&flow.id)
        .bind(&flow.kind)
        .bind(&flow.region)
        .bind(&flow.status)
        .bind(&flow.authorize_url)
        .bind(&flow.redirect_uri)
        .bind(&flow.ticket_id)
        .bind(&flow.secrets_encrypted)
        .bind(&flow.requested_name)
        .bind(&flow.account_id)
        .bind(&flow.error)
        .bind(encode_ts(flow.created_at))
        .bind(encode_ts(flow.expires_at))
        .bind(flow.completed_at.map(encode_ts))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn flow(&self, id: &str) -> Result<Option<AuthFlowRow>, GatewayError> {
        let query = format!("SELECT {FLOW_COLUMNS} FROM admin_auth_flows WHERE id = ?");
        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_flow(&row)).transpose()
    }

    /// The source protocol returns only `code` on the callback, so at most one
    /// authorization may be pending at a time.
    pub async fn active_flow(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<AuthFlowRow>, GatewayError> {
        let query = format!(
            "SELECT {FLOW_COLUMNS} FROM admin_auth_flows \
             WHERE status IN ('pending', 'exchanging') AND expires_at > ? \
             ORDER BY created_at DESC LIMIT 1"
        );
        let row = sqlx::query(&query)
            .bind(encode_ts(now))
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_flow(&row)).transpose()
    }

    /// Atomically take ownership of a pending flow before exchanging the code.
    pub async fn claim_flow(&self, id: &str, now: DateTime<Utc>) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE admin_auth_flows SET status = 'exchanging' \
             WHERE id = ? AND status = 'pending' AND expires_at > ?",
        )
        .bind(id)
        .bind(encode_ts(now))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn finish_flow(
        &self,
        id: &str,
        status: &str,
        account_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), GatewayError> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE admin_auth_flows SET status = ?, account_id = ?, error = ?, \
             completed_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(account_id)
        .bind(error)
        .bind(encode_ts(now))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn cancel_flow(&self, id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE admin_auth_flows SET status = 'cancelled', completed_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(encode_ts(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn expire_flows(&self, now: DateTime<Utc>) -> Result<(), GatewayError> {
        sqlx::query(
            "UPDATE admin_auth_flows SET status = 'expired', completed_at = ? \
             WHERE status IN ('pending', 'exchanging') AND expires_at <= ?",
        )
        .bind(encode_ts(now))
        .bind(encode_ts(now))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }
}

fn map_flow(row: &sqlx::sqlite::SqliteRow) -> Result<AuthFlowRow, GatewayError> {
    let created_raw: String = row.try_get("created_at")?;
    let expires_raw: String = row.try_get("expires_at")?;
    let completed_raw: Option<String> = row.try_get("completed_at")?;
    Ok(AuthFlowRow {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        region: row.try_get("region")?,
        status: row.try_get("status")?,
        authorize_url: row.try_get("authorize_url")?,
        redirect_uri: row.try_get("redirect_uri")?,
        ticket_id: row.try_get("ticket_id")?,
        secrets_encrypted: row.try_get("secrets_encrypted")?,
        requested_name: row.try_get("requested_name")?,
        account_id: row.try_get("account_id")?,
        error: row.try_get("error")?,
        created_at: decode_ts(&created_raw)?,
        expires_at: decode_ts(&expires_raw)?,
        completed_at: completed_raw.as_deref().map(decode_ts).transpose()?,
    })
}
