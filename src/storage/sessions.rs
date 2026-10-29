//! Session binding persistence (no affinity algorithm here).

use chrono::{Duration, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;
use crate::sessions::{Session, SessionState};

use super::{decode_ts, encode_ts, storage_error};

const COLUMNS: &str = "id, client_session_id, provider, provider_kind, region, account_id, \
     upstream_session_id, state, binding_revision, invalid_reason, created_at, last_used_at, \
     expires_at, request_count, metadata";

#[derive(Clone)]
pub struct SessionRepo {
    pool: SqlitePool,
}

impl SessionRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert or update the binding for `(client_session_id, provider_kind,
    /// region)`. An empty `session.id` is replaced with a generated uuid.
    pub async fn save(&self, session: &Session) -> Result<Session, GatewayError> {
        let id = if session.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            session.id.clone()
        };
        sqlx::query(
            "INSERT INTO sessions (id, client_session_id, provider, provider_kind, region, \
             account_id, upstream_session_id, state, binding_revision, invalid_reason, created_at, \
             last_used_at, expires_at, request_count, metadata) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(client_session_id, provider_kind, region) DO UPDATE SET \
             provider = excluded.provider, account_id = excluded.account_id, \
             upstream_session_id = excluded.upstream_session_id, state = excluded.state, \
             invalid_reason = excluded.invalid_reason, \
             last_used_at = excluded.last_used_at, expires_at = excluded.expires_at, \
             metadata = excluded.metadata",
        )
        .bind(&id)
        .bind(&session.client_session_id)
        .bind(&session.provider)
        .bind(session.kind.as_str())
        .bind(&session.region)
        .bind(&session.account_id)
        .bind(&session.upstream_session_id)
        .bind(session.state.as_str())
        .bind(session.binding_revision)
        .bind(&session.invalid_reason)
        .bind(encode_ts(session.created_at))
        .bind(encode_ts(session.last_used_at))
        .bind(encode_ts(session.expires_at))
        .bind(session.request_count as i64)
        .bind(session.metadata.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;

        let stored = self
            .get(
                &session.client_session_id,
                session.kind,
                Region::from_db_str(&session.region).ok_or_else(|| {
                    GatewayError::bad_request(format!("unknown region {}", session.region))
                })?,
            )
            .await?;
        stored.ok_or_else(|| GatewayError::internal("session disappeared after save"))
    }

    /// Insert a binding only when `(client_session_id, kind, region)` does not
    /// exist yet. Returns `false` on conflict, leaving the existing binding
    /// untouched (this is what keeps a concurrent request from overwriting an
    /// established account/session mapping).
    pub async fn insert_if_absent(&self, session: &Session) -> Result<bool, GatewayError> {
        let id = if session.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            session.id.clone()
        };
        let result = sqlx::query(
            "INSERT INTO sessions (id, client_session_id, provider, provider_kind, region, \
             account_id, upstream_session_id, state, binding_revision, invalid_reason, created_at, \
             last_used_at, expires_at, request_count, metadata) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(client_session_id, provider_kind, region) DO NOTHING",
        )
        .bind(&id)
        .bind(&session.client_session_id)
        .bind(&session.provider)
        .bind(session.kind.as_str())
        .bind(&session.region)
        .bind(&session.account_id)
        .bind(&session.upstream_session_id)
        .bind(session.state.as_str())
        .bind(session.binding_revision)
        .bind(&session.invalid_reason)
        .bind(encode_ts(session.created_at))
        .bind(encode_ts(session.last_used_at))
        .bind(encode_ts(session.expires_at))
        .bind(session.request_count as i64)
        .bind(session.metadata.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Attach the upstream session id only if the caller still observes the same
    /// account and binding revision. Returns `None` on a stale/mismatched write.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_upstream(
        &self,
        client_session_id: &str,
        kind: ProviderKind,
        region: Region,
        account_id: &str,
        expected_revision: i64,
        upstream_session_id: &str,
        ttl_secs: u64,
    ) -> Result<Option<Session>, GatewayError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE sessions SET upstream_session_id = ?, binding_revision = binding_revision + 1, \
             last_used_at = ?, expires_at = ? \
             WHERE client_session_id = ? AND provider_kind = ? AND region = ? \
             AND account_id = ? AND binding_revision = ? AND state = 'active'",
        )
        .bind(upstream_session_id)
        .bind(encode_ts(now))
        .bind(encode_ts(now + Duration::seconds(ttl_secs as i64)))
        .bind(client_session_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(account_id)
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get(client_session_id, kind, region).await
    }

    /// Move a binding to another verified account. The upstream id is replaced
    /// (never carried across accounts); callers must supply the new one or
    /// `None`, and `expected_revision` prevents a stale scheduler from moving a
    /// session that changed in the meantime.
    #[allow(clippy::too_many_arguments)]
    pub async fn rebind(
        &self,
        client_session_id: &str,
        kind: ProviderKind,
        region: Region,
        expected_revision: i64,
        account_id: &str,
        provider: &str,
        upstream_session_id: Option<&str>,
        ttl_secs: u64,
    ) -> Result<Option<Session>, GatewayError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE sessions SET account_id = ?, provider = ?, upstream_session_id = ?, \
             state = 'active', invalid_reason = NULL, binding_revision = binding_revision + 1, \
             last_used_at = ?, expires_at = ? \
             WHERE client_session_id = ? AND provider_kind = ? AND region = ? \
             AND binding_revision = ?",
        )
        .bind(account_id)
        .bind(provider)
        .bind(upstream_session_id)
        .bind(encode_ts(now))
        .bind(encode_ts(now + Duration::seconds(ttl_secs as i64)))
        .bind(client_session_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get(client_session_id, kind, region).await
    }

    /// Mark the binding invalid while keeping the account and upstream id for
    /// audit. Acquisition must fail until an explicit recovery action.
    pub async fn invalidate(
        &self,
        client_session_id: &str,
        kind: ProviderKind,
        region: Region,
        expected_revision: i64,
        reason: &str,
    ) -> Result<Option<Session>, GatewayError> {
        let result = sqlx::query(
            "UPDATE sessions SET state = 'invalid', invalid_reason = ?, \
             binding_revision = binding_revision + 1 \
             WHERE client_session_id = ? AND provider_kind = ? AND region = ? \
             AND binding_revision = ?",
        )
        .bind(reason)
        .bind(client_session_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get(client_session_id, kind, region).await
    }

    /// Recovery for an upstream session the same account can no longer use:
    /// keep the account, drop only the upstream id, and record why. The adapter
    /// must resend the full history on the next request.
    #[allow(clippy::too_many_arguments)]
    pub async fn reset_upstream(
        &self,
        client_session_id: &str,
        kind: ProviderKind,
        region: Region,
        account_id: &str,
        expected_revision: i64,
        reason: &str,
        ttl_secs: u64,
    ) -> Result<Option<Session>, GatewayError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE sessions SET upstream_session_id = NULL, state = 'active', \
             invalid_reason = ?, binding_revision = binding_revision + 1, \
             last_used_at = ?, expires_at = ? \
             WHERE client_session_id = ? AND provider_kind = ? AND region = ? \
             AND account_id = ? AND binding_revision = ?",
        )
        .bind(reason)
        .bind(encode_ts(now))
        .bind(encode_ts(now + Duration::seconds(ttl_secs as i64)))
        .bind(client_session_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(account_id)
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get(client_session_id, kind, region).await
    }

    pub async fn get(
        &self,
        client_session_id: &str,
        kind: ProviderKind,
        region: Region,
    ) -> Result<Option<Session>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM sessions \
             WHERE client_session_id = ? AND provider_kind = ? AND region = ?"
        );
        let row = sqlx::query(&query)
            .bind(client_session_id)
            .bind(kind.as_str())
            .bind(region.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_session(&row)).transpose()
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Option<Session>, GatewayError> {
        let query = format!("SELECT {COLUMNS} FROM sessions WHERE id = ?");
        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_session(&row)).transpose()
    }

    pub async fn list_by_account(&self, account_id: &str) -> Result<Vec<Session>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM sessions WHERE account_id = ? ORDER BY last_used_at DESC"
        );
        let rows = sqlx::query(&query)
            .bind(account_id)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_session).collect()
    }

    /// Count requests for this binding and extend its expiry in one atomic
    /// statement (`request_count = request_count + 1`).
    pub async fn touch(&self, id: &str, ttl_secs: u64) -> Result<bool, GatewayError> {
        let now = Utc::now();
        let expires_at = now + Duration::seconds(ttl_secs as i64);
        let result = sqlx::query(
            "UPDATE sessions SET request_count = request_count + 1, last_used_at = ?, \
             expires_at = ? WHERE id = ? AND state = 'active'",
        )
        .bind(encode_ts(now))
        .bind(encode_ts(expires_at))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_state(&self, id: &str, state: SessionState) -> Result<bool, GatewayError> {
        let result = sqlx::query("UPDATE sessions SET state = ? WHERE id = ?")
            .bind(state.as_str())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Session assignments (not HTTP requests, not tokens) for one account.
    pub async fn count_active_by_account(&self, account_id: &str) -> Result<i64, GatewayError> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM sessions WHERE account_id = ? AND state = 'active'",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)
    }

    /// Remove expired bindings; returns how many rows were deleted.
    pub async fn delete_expired(&self) -> Result<u64, GatewayError> {
        let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(encode_ts(Utc::now()))
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected())
    }
}

fn map_session(row: &sqlx::sqlite::SqliteRow) -> Result<Session, GatewayError> {
    let kind_raw: String = row.try_get("provider_kind")?;
    let kind = ProviderKind::from_db_str(&kind_raw)
        .ok_or_else(|| GatewayError::storage("unknown provider kind stored in database"))?;
    let state_raw: String = row.try_get("state")?;
    let metadata_raw: String = row.try_get("metadata")?;
    let created_raw: String = row.try_get("created_at")?;
    let used_raw: String = row.try_get("last_used_at")?;
    let expires_raw: String = row.try_get("expires_at")?;

    Ok(Session {
        id: row.try_get("id")?,
        client_session_id: row.try_get("client_session_id")?,
        provider: row.try_get("provider")?,
        kind,
        region: row.try_get("region")?,
        account_id: row.try_get("account_id")?,
        upstream_session_id: row.try_get("upstream_session_id")?,
        state: SessionState::from_db_str(&state_raw),
        binding_revision: row.try_get("binding_revision")?,
        invalid_reason: row.try_get("invalid_reason")?,
        created_at: decode_ts(&created_raw)?,
        last_used_at: decode_ts(&used_raw)?,
        expires_at: decode_ts(&expires_raw)?,
        request_count: row.try_get::<i64, _>("request_count")? as u64,
        metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| serde_json::json!({})),
    })
}
