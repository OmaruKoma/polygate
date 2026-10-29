//! Encrypted credential storage.
//!
//! The repository only ever writes `SecretBox` ciphertext; plaintext exists in
//! memory for the duration of one call and is zeroized afterwards.

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::error::GatewayError;
use crate::security::{SecretBox, ENCRYPTION_VERSION};

use super::{decode_ts, encode_ts, storage_error};

/// Non-secret metadata for one credential row.
#[derive(Debug, Clone)]
pub struct CredentialInfo {
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub encryption_version: String,
}

/// Upsert ciphertext; also used inside the account creation transaction.
pub(crate) async fn upsert_payload<'e, E>(
    executor: E,
    account_id: &str,
    payload: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let now = encode_ts(now);
    sqlx::query(
        "INSERT INTO credentials (account_id, encrypted_payload, encryption_version, \
         created_at, updated_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(account_id) DO UPDATE SET encrypted_payload = excluded.encrypted_payload, \
         encryption_version = excluded.encryption_version, updated_at = excluded.updated_at, \
         revision = credentials.revision + 1",
    )
    .bind(account_id)
    .bind(payload)
    .bind(ENCRYPTION_VERSION)
    .bind(&now)
    .bind(&now)
    .execute(executor)
    .await?;
    Ok(())
}

pub(crate) fn encrypt_credential(
    secret_box: &SecretBox,
    account_id: &str,
    credential: &Credential,
) -> Result<String, GatewayError> {
    let mut plaintext = serde_json::to_vec(credential.to_json())
        .map_err(|_| GatewayError::internal("credential serialization failed"))?;
    let payload = secret_box.encrypt_for_account(account_id, &plaintext);
    plaintext.zeroize();
    payload
}

#[derive(Clone)]
pub struct CredentialRepo {
    pool: SqlitePool,
}

impl CredentialRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Encrypt with AAD bound to `account_id` and upsert. The account must
    /// exist (foreign key). This is an explicit replace; the service layer uses
    /// [`Self::insert_if_absent`] / [`Self::update_if_revision`] for lifecycle
    /// rules.
    pub async fn save(
        &self,
        secret_box: &SecretBox,
        account_id: &str,
        credential: &Credential,
    ) -> Result<(), GatewayError> {
        let payload = encrypt_credential(secret_box, account_id, credential)?;
        upsert_payload(&self.pool, account_id, &payload, Utc::now())
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    /// Insert only if no credential row exists. Returns `false` on conflict.
    pub async fn insert_if_absent(
        &self,
        secret_box: &SecretBox,
        account_id: &str,
        credential: &Credential,
    ) -> Result<bool, GatewayError> {
        let payload = encrypt_credential(secret_box, account_id, credential)?;
        let now = encode_ts(Utc::now());
        let result = sqlx::query(
            "INSERT INTO credentials (account_id, encrypted_payload, encryption_version, \
             created_at, updated_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(account_id) DO NOTHING",
        )
        .bind(account_id)
        .bind(payload)
        .bind(ENCRYPTION_VERSION)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Compare-and-swap update. Returns the new revision, or `None` when the
    /// row is missing or another writer already advanced beyond `expected`.
    pub async fn update_if_revision(
        &self,
        secret_box: &SecretBox,
        account_id: &str,
        credential: &Credential,
        expected: i64,
    ) -> Result<Option<i64>, GatewayError> {
        let payload = encrypt_credential(secret_box, account_id, credential)?;
        let result = sqlx::query(
            "UPDATE credentials SET encrypted_payload = ?, encryption_version = ?, \
             updated_at = ?, revision = revision + 1 \
             WHERE account_id = ? AND revision = ?",
        )
        .bind(payload)
        .bind(ENCRYPTION_VERSION)
        .bind(encode_ts(Utc::now()))
        .bind(account_id)
        .bind(expected)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() == 1 {
            Ok(Some(expected + 1))
        } else {
            Ok(None)
        }
    }

    /// Non-secret metadata. `None` when no credential row exists.
    pub async fn info(&self, account_id: &str) -> Result<Option<CredentialInfo>, GatewayError> {
        let row = sqlx::query(
            "SELECT revision, created_at, updated_at, encryption_version \
             FROM credentials WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let created_raw: String = row.try_get("created_at")?;
        let updated_raw: String = row.try_get("updated_at")?;
        Ok(Some(CredentialInfo {
            revision: row.try_get("revision")?,
            created_at: decode_ts(&created_raw)?,
            updated_at: decode_ts(&updated_raw)?,
            encryption_version: row.try_get("encryption_version")?,
        }))
    }

    pub async fn exists(&self, account_id: &str) -> Result<bool, GatewayError> {
        let row = sqlx::query("SELECT 1 FROM credentials WHERE account_id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(row.is_some())
    }

    /// Take the short-lived refresh lease for one account. `true` means this
    /// caller may talk to the refresh endpoint; a live lease owned by anyone
    /// else (including another process) returns `false`.
    pub async fn acquire_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<bool, GatewayError> {
        let expires_at = now.saturating_add(ttl_seconds.max(1));
        let result = sqlx::query(
            "INSERT INTO credential_refresh_leases (account_id, owner, expires_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(account_id) DO UPDATE SET owner = excluded.owner, \
             expires_at = excluded.expires_at \
             WHERE credential_refresh_leases.expires_at <= ? \
                OR credential_refresh_leases.owner = ?",
        )
        .bind(account_id)
        .bind(owner)
        .bind(expires_at)
        .bind(now)
        .bind(owner)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Release a lease only if this owner still holds it.
    pub async fn release_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
    ) -> Result<(), GatewayError> {
        sqlx::query("DELETE FROM credential_refresh_leases WHERE account_id = ? AND owner = ?")
            .bind(account_id)
            .bind(owner)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    /// Decrypt for one account. Returns `None` when no credential is stored.
    pub async fn load(
        &self,
        secret_box: &SecretBox,
        account_id: &str,
    ) -> Result<Option<Credential>, GatewayError> {
        let row = sqlx::query(
            "SELECT encrypted_payload, encryption_version FROM credentials WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let version: String = row.try_get("encryption_version")?;
        if version != ENCRYPTION_VERSION {
            return Err(GatewayError::storage(
                "unsupported credential encryption version",
            ));
        }
        let payload: String = row.try_get("encrypted_payload")?;

        let mut plaintext = secret_box.decrypt_for_account(account_id, &payload)?;
        let parsed = serde_json::from_slice(&plaintext)
            .map_err(|_| GatewayError::storage("stored credential is not valid JSON"));
        plaintext.zeroize();
        Ok(Some(Credential::from_value(parsed?)))
    }

    pub async fn delete(&self, account_id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM credentials WHERE account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Ciphertext only. Used by tests; never decrypted here.
    #[cfg(test)]
    pub(crate) async fn raw_payload(
        &self,
        account_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        let row = sqlx::query("SELECT encrypted_payload FROM credentials WHERE account_id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| row.try_get("encrypted_payload").map_err(Into::into))
            .transpose()
    }
}
