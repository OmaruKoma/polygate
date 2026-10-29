//! Account metadata persistence (never the secret itself).

use chrono::Utc;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::accounts::{Account, AuthStatus, NewAccount};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;
use crate::security::credential_hint;

use super::{decode_ts, encode_ts, storage_error};

const COLUMNS: &str = "id, provider, provider_kind, region, display_name, credential_hint, \
     external_id, enabled, weight, max_concurrency, quota_group, auth_status, auth_checked_at, \
     last_error, metadata, created_at, updated_at";

#[derive(Clone)]
pub struct AccountRepo {
    pool: SqlitePool,
}

impl AccountRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, new: &NewAccount) -> Result<Account, GatewayError> {
        let id = Uuid::new_v4().to_string();
        insert_row(&self.pool, &id, new, Utc::now())
            .await
            .map_err(storage_error)?;
        self.get(&id)
            .await?
            .ok_or_else(|| GatewayError::internal("account disappeared after insert"))
    }

    pub async fn get(&self, id: &str) -> Result<Option<Account>, GatewayError> {
        let query = format!("SELECT {COLUMNS} FROM accounts WHERE id = ?");
        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_account(&row)).transpose()
    }

    pub async fn list(&self) -> Result<Vec<Account>, GatewayError> {
        let query = format!("SELECT {COLUMNS} FROM accounts ORDER BY created_at, id");
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_account).collect()
    }

    /// Find one account by its upstream identity inside a kind/region.
    pub async fn find_by_external(
        &self,
        kind: ProviderKind,
        region: Region,
        external_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        find_by_external(&self.pool, kind, region, external_id)
            .await
            .map_err(storage_error)
    }

    pub async fn update(&self, account: &Account) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE accounts SET provider = ?, provider_kind = ?, region = ?, display_name = ?, \
             credential_hint = ?, external_id = ?, enabled = ?, weight = ?, max_concurrency = ?, \
             quota_group = ?, auth_status = ?, auth_checked_at = ?, last_error = ?, metadata = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(&account.provider)
        .bind(account.kind.as_str())
        .bind(account.region.as_str())
        .bind(&account.name)
        .bind(&account.credential_hint)
        .bind(&account.external_id)
        .bind(flag(account.enabled))
        .bind(i64::from(account.weight))
        .bind(i64::from(account.max_concurrency))
        .bind(&account.quota_group)
        .bind(account.auth_status.as_str())
        .bind(account.auth_checked_at.map(encode_ts))
        .bind(&account.last_error)
        .bind(account.metadata.to_string())
        .bind(encode_ts(Utc::now()))
        .bind(&account.id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Record an auth probe result in one atomic statement.
    pub async fn update_auth(
        &self,
        id: &str,
        status: AuthStatus,
        last_error: Option<&str>,
    ) -> Result<bool, GatewayError> {
        let now = Utc::now();
        let result = sqlx::query(
            "UPDATE accounts SET auth_status = ?, auth_checked_at = ?, last_error = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(encode_ts(now))
        .bind(last_error)
        .bind(encode_ts(now))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, GatewayError> {
        let result = sqlx::query("UPDATE accounts SET enabled = ?, updated_at = ? WHERE id = ?")
            .bind(flag(enabled))
            .bind(encode_ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Deletes the account; credentials, sessions and quota history cascade.
    pub async fn delete(&self, id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }
}

/// Insert used both standalone and inside a transaction owned by
/// [`crate::storage::Storage::create_account`].
pub(crate) async fn insert_row<'e, E>(
    executor: E,
    id: &str,
    new: &NewAccount,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let region = new.region.unwrap_or(Region::Cn);
    sqlx::query(
        "INSERT INTO accounts (id, provider, provider_kind, region, display_name, \
         credential_hint, external_id, enabled, weight, max_concurrency, quota_group, auth_status, \
         metadata, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'unknown', ?, ?, ?)",
    )
    .bind(id)
    .bind(&new.provider)
    .bind(new.kind.as_str())
    .bind(region.as_str())
    .bind(&new.name)
    .bind(credential_hint(&new.credential))
    .bind(&new.external_id)
    .bind(flag(new.enabled))
    .bind(i64::from(new.weight))
    .bind(i64::from(new.max_concurrency))
    .bind(&new.quota_group)
    .bind(new.metadata.to_string())
    .bind(encode_ts(now))
    .bind(encode_ts(now))
    .execute(executor)
    .await?;
    Ok(())
}

/// Find the account that already owns an import identity. Scoped by
/// `(kind, region)` so the same upstream user in CN and Intl stays separate.
pub(crate) async fn find_by_external<'e, E>(
    executor: E,
    kind: ProviderKind,
    region: Region,
    external_id: &str,
) -> Result<Option<String>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let row = sqlx::query(
        "SELECT id FROM accounts WHERE provider_kind = ? AND region = ? AND external_id = ?",
    )
    .bind(kind.as_str())
    .bind(region.as_str())
    .bind(external_id)
    .fetch_optional(executor)
    .await?;
    row.map(|row| row.try_get("id")).transpose()
}

fn map_account(row: &sqlx::sqlite::SqliteRow) -> Result<Account, GatewayError> {
    let kind_raw: String = row.try_get("provider_kind")?;
    let kind = ProviderKind::from_db_str(&kind_raw)
        .ok_or_else(|| GatewayError::storage("unknown provider kind stored in database"))?;
    let region_raw: String = row.try_get("region")?;
    let region = Region::from_db_str(&region_raw)
        .ok_or_else(|| GatewayError::storage("unknown region stored in database"))?;
    let auth_raw: String = row.try_get("auth_status")?;
    let checked_raw: Option<String> = row.try_get("auth_checked_at")?;
    let metadata_raw: String = row.try_get("metadata")?;
    let created_raw: String = row.try_get("created_at")?;
    let updated_raw: String = row.try_get("updated_at")?;

    Ok(Account {
        id: row.try_get("id")?,
        provider: row.try_get("provider")?,
        kind,
        region,
        name: row.try_get("display_name")?,
        credential_hint: row.try_get("credential_hint")?,
        external_id: row.try_get("external_id")?,
        enabled: row.try_get::<i64, _>("enabled")? != 0,
        weight: row.try_get::<i64, _>("weight")? as u32,
        max_concurrency: row.try_get::<i64, _>("max_concurrency")? as u32,
        quota_group: row.try_get("quota_group")?,
        auth_status: AuthStatus::from_db_str(&auth_raw),
        auth_checked_at: checked_raw.as_deref().map(decode_ts).transpose()?,
        last_error: row.try_get("last_error")?,
        metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| serde_json::json!({})),
        created_at: decode_ts(&created_raw)?,
        updated_at: decode_ts(&updated_raw)?,
    })
}

fn flag(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}
