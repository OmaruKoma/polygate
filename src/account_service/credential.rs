//! Credential lifecycle service.
//!
//! Secrets only exist in memory between [`SecretBox`] and the repository. The
//! repository encrypts with AAD bound to the account id; this service never
//! logs or serializes plaintext. Updates use an optimistic revision check so a
//! stale writer cannot silently overwrite a newer credential.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::accounts::{AuthStatus, Credential};
use crate::error::{ErrorKind, GatewayError};
use crate::security::SecretBox;
use crate::storage::{CredentialInfo, Storage};

/// Public, non-secret metadata for one account's credential.
#[derive(Debug, Clone)]
pub struct CredentialStatus {
    pub account_id: String,
    pub present: bool,
    pub revision: Option<i64>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub encryption_version: Option<String>,
    pub auth_status: AuthStatus,
}

#[derive(Clone)]
pub struct CredentialService {
    storage: Storage,
    secret_box: Arc<SecretBox>,
}

impl CredentialService {
    pub fn new(storage: Storage, secret_box: Arc<SecretBox>) -> Self {
        Self {
            storage,
            secret_box,
        }
    }

    /// Create a credential for an existing account. Fails if one is already
    /// stored: replacing an existing secret must go through [`Self::update`]
    /// with the current revision, so two admin sessions cannot overwrite each
    /// other silently.
    pub async fn save(
        &self,
        account_id: &str,
        credential: &Credential,
    ) -> Result<CredentialStatus, GatewayError> {
        self.ensure_account(account_id).await?;
        validate_credential(credential)?;
        let inserted = self
            .storage
            .credentials()
            .insert_if_absent(&self.secret_box, account_id, credential)
            .await?;
        if !inserted {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "credential already exists; use update with the current revision",
            ));
        }
        self.status(account_id)
            .await?
            .ok_or_else(|| GatewayError::internal("account disappeared after credential save"))
    }

    /// Replace a credential only if the caller observed `expected_revision`.
    /// A mismatch means another writer won; the caller should reload and retry.
    pub async fn update(
        &self,
        account_id: &str,
        credential: &Credential,
        expected_revision: i64,
    ) -> Result<CredentialStatus, GatewayError> {
        self.ensure_account(account_id).await?;
        validate_credential(credential)?;
        if !self.storage.credentials().exists(account_id).await? {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "credential not found; save it before updating",
            ));
        }
        let updated = self
            .storage
            .credentials()
            .update_if_revision(&self.secret_box, account_id, credential, expected_revision)
            .await?;
        if updated.is_none() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "credential was modified concurrently; reload its revision and retry",
            ));
        }
        self.status(account_id)
            .await?
            .ok_or_else(|| GatewayError::internal("account disappeared after credential update"))
    }

    /// Decrypt one account's credential. Returns `None` when the account has
    /// no stored secret. There is deliberately no "list all credentials" API.
    pub async fn load(&self, account_id: &str) -> Result<Option<Credential>, GatewayError> {
        self.storage
            .credentials()
            .load(&self.secret_box, account_id)
            .await
    }

    /// Remove the secret but keep the account, its auth status and its local
    /// enabled flag. Returns `false` when no credential existed.
    pub async fn delete(&self, account_id: &str) -> Result<bool, GatewayError> {
        self.storage.credentials().delete(account_id).await
    }

    /// Cross-process refresh lease. `false` means another gateway process owns
    /// the refresh for this account right now, so the caller must not call the
    /// upstream refresh endpoint.
    pub async fn acquire_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<bool, GatewayError> {
        self.storage
            .credentials()
            .acquire_refresh_lease(account_id, owner, now, ttl_seconds)
            .await
    }

    pub async fn release_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
    ) -> Result<(), GatewayError> {
        self.storage
            .credentials()
            .release_refresh_lease(account_id, owner)
            .await
    }

    /// Non-secret metadata, including the account-level auth status. `None`
    /// means the account itself does not exist.
    pub async fn status(&self, account_id: &str) -> Result<Option<CredentialStatus>, GatewayError> {
        let Some(account) = self.storage.accounts().get(account_id).await? else {
            return Ok(None);
        };
        let info: Option<CredentialInfo> = self.storage.credentials().info(account_id).await?;
        Ok(Some(CredentialStatus {
            account_id: account.id,
            present: info.is_some(),
            revision: info.as_ref().map(|value| value.revision),
            created_at: info.as_ref().map(|value| value.created_at),
            updated_at: info.as_ref().map(|value| value.updated_at),
            encryption_version: info.map(|value| value.encryption_version),
            auth_status: account.auth_status,
        }))
    }

    /// Mark the credential invalid without deleting it and without changing
    /// the local enabled flag. The account keeps existing for audit/re-login.
    pub async fn invalidate(
        &self,
        account_id: &str,
        reason: &str,
    ) -> Result<AuthStatus, GatewayError> {
        self.require_present(account_id).await?;
        self.set_auth_status(account_id, AuthStatus::Invalid, Some(reason))
            .await
    }

    /// Mark that the credential must be refreshed before normal dispatch.
    pub async fn mark_refresh_required(
        &self,
        account_id: &str,
        reason: &str,
    ) -> Result<AuthStatus, GatewayError> {
        self.require_present(account_id).await?;
        self.set_auth_status(account_id, AuthStatus::RefreshRequired, Some(reason))
            .await
    }

    /// Persist an auth transition after checking the transition table. This is
    /// shared by [`crate::accounts::AccountService`] so both services enforce
    /// the same rules.
    pub async fn set_auth_status(
        &self,
        account_id: &str,
        status: AuthStatus,
        last_error: Option<&str>,
    ) -> Result<AuthStatus, GatewayError> {
        let account = self
            .storage
            .accounts()
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::new(ErrorKind::BadRequest, "account not found"))?;
        if !account.auth_status.can_transition_to(status) {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                format!(
                    "invalid auth status transition: {} -> {}",
                    account.auth_status.as_str(),
                    status.as_str()
                ),
            ));
        }
        let reason = last_error.map(truncate_reason);
        let updated = self
            .storage
            .accounts()
            .update_auth(account_id, status, reason.as_deref())
            .await?;
        if !updated {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account not found",
            ));
        }
        Ok(status)
    }

    async fn ensure_account(&self, account_id: &str) -> Result<(), GatewayError> {
        if self.storage.accounts().get(account_id).await?.is_none() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account not found",
            ));
        }
        Ok(())
    }

    async fn require_present(&self, account_id: &str) -> Result<(), GatewayError> {
        self.ensure_account(account_id).await?;
        if !self.storage.credentials().exists(account_id).await? {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account has no credential",
            ));
        }
        Ok(())
    }
}

fn validate_credential(credential: &Credential) -> Result<(), GatewayError> {
    if !credential.to_json().is_object() {
        return Err(GatewayError::new(
            ErrorKind::BadRequest,
            "credential must be a JSON object",
        ));
    }
    Ok(())
}

fn truncate_reason(reason: &str) -> String {
    const MAX: usize = 512;
    if reason.chars().count() <= MAX {
        return reason.to_string();
    }
    let mut out: String = reason.chars().take(MAX).collect();
    out.push('…');
    out
}
