//! Account lifecycle service.
//!
//! This layer owns the policy that is too important to repeat in HTTP handlers,
//! schedulers and provider adapters:
//!
//! * account identity is a UUID, never a phone number or display name;
//! * `kind` + `region` are immutable after creation, so a CN credential can
//!   never be selected for an International request;
//! * local `enabled` and upstream [`AuthStatus`] are independent;
//! * auth failures are classified; a timeout or transport error does not turn
//!   a credential into `Invalid`;
//! * credentials are encrypted and revision-checked by [`CredentialService`].

use std::fmt;
use std::sync::Arc;

use crate::accounts::{Account, AuthStatus, Credential, NewAccount};
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::security::SecretBox;
use crate::storage::Storage;

use super::{CredentialService, CredentialStatus};

/// Input for creating one account. `provider` is the provider instance id
/// (for example `codearts-cn`); identity is always the generated account UUID.
#[derive(Clone)]
pub struct CreateAccount {
    pub provider: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub name: String,
    /// Stable upstream identity, unique within `(kind, region)`.
    pub external_id: Option<String>,
    /// `None` creates an account without a secret; a credential can be added
    /// later with [`CredentialService::save`].
    pub credential: Option<serde_json::Value>,
    pub weight: u32,
    pub max_concurrency: u32,
    pub quota_group: Option<String>,
    pub enabled: bool,
    pub metadata: serde_json::Value,
}

impl CreateAccount {
    pub fn new(
        kind: ProviderKind,
        region: Region,
        name: impl Into<String>,
        credential: Option<serde_json::Value>,
    ) -> Self {
        Self {
            provider: format!("{}-{}", kind.as_str(), region.as_str()),
            kind,
            region,
            name: name.into(),
            external_id: None,
            credential,
            weight: 1,
            max_concurrency: 4,
            quota_group: None,
            enabled: true,
            metadata: serde_json::json!({}),
        }
    }
}

impl fmt::Debug for CreateAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateAccount")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("region", &self.region)
            .field("name", &self.name)
            .field("external_id", &self.external_id)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .field("weight", &self.weight)
            .field("max_concurrency", &self.max_concurrency)
            .field("quota_group", &self.quota_group)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// Mutable, non-identity account fields. `kind`, `region`, `provider` and the
/// account id are intentionally absent: changing them would invalidate the
/// credential AAD, sessions and quota history.
#[derive(Debug, Clone, Default)]
pub struct AccountUpdate {
    pub name: Option<String>,
    pub weight: Option<u32>,
    pub max_concurrency: Option<u32>,
    /// `None` = leave unchanged; `Some(None)` = clear the quota group.
    pub quota_group: Option<Option<String>>,
    pub metadata: Option<serde_json::Value>,
}

/// Authorization data handed to a provider/scheduler/check-in task.
///
/// The struct contains a decrypted secret; its `Debug` implementation never
/// prints it. Callers should drop it as soon as the request/check-in finishes.
pub struct AuthorizationContext {
    pub account_id: String,
    pub provider: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub enabled: bool,
    pub auth_status: AuthStatus,
    pub credential: Option<Credential>,
    pub credential_revision: Option<i64>,
}

impl fmt::Debug for AuthorizationContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationContext")
            .field("account_id", &self.account_id)
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("region", &self.region)
            .field("enabled", &self.enabled)
            .field("auth_status", &self.auth_status)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .field("credential_revision", &self.credential_revision)
            .finish()
    }
}

#[derive(Clone)]
pub struct AccountService {
    storage: Storage,
    secret_box: Arc<SecretBox>,
    credentials: CredentialService,
}

impl AccountService {
    pub fn new(storage: Storage, secret_box: Arc<SecretBox>) -> Self {
        Self {
            credentials: CredentialService::new(storage.clone(), Arc::clone(&secret_box)),
            storage,
            secret_box,
        }
    }

    pub fn from_secret_box(storage: Storage, secret_box: SecretBox) -> Self {
        Self::new(storage, Arc::new(secret_box))
    }

    pub fn credentials(&self) -> &CredentialService {
        &self.credentials
    }

    /// Create an account. When a credential is supplied it is encrypted and
    /// written in the same transaction as the account row; without a credential
    /// only the account row is created.
    ///
    /// Duplicate display names are allowed: the account id (UUID) is the
    /// identity, so two providers/accounts may share a human label. Nothing here
    /// treats a phone number or a name as a unique key.
    pub async fn create(&self, request: CreateAccount) -> Result<Account, GatewayError> {
        let CreateAccount {
            provider,
            kind,
            region,
            name,
            external_id,
            credential,
            weight,
            max_concurrency,
            quota_group,
            enabled,
            metadata,
        } = request;

        let provider = provider.trim().to_string();
        if provider.is_empty() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "provider instance id must not be empty",
            ));
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account name must not be empty",
            ));
        }
        if weight == 0 {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account weight must be at least 1",
            ));
        }
        if max_concurrency == 0 {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account max_concurrency must be at least 1",
            ));
        }
        let external_id = match external_id {
            Some(value) => {
                let value = value.trim().to_string();
                if value.is_empty() || value.chars().count() > 256 {
                    return Err(GatewayError::new(
                        ErrorKind::BadRequest,
                        "account external_id must be 1..=256 characters",
                    ));
                }
                if self
                    .storage
                    .accounts()
                    .find_by_external(kind, region, &value)
                    .await?
                    .is_some()
                {
                    return Err(GatewayError::new(
                        ErrorKind::BadRequest,
                        "an account with this external identity already exists",
                    ));
                }
                Some(value)
            }
            None => None,
        };
        let (credential_value, has_credential) = match credential {
            Some(value) => {
                if !value.is_object() {
                    return Err(GatewayError::new(
                        ErrorKind::BadRequest,
                        "credential must be a JSON object",
                    ));
                }
                (value, true)
            }
            None => (serde_json::json!({}), false),
        };

        let new = NewAccount {
            provider,
            kind,
            region: Some(region),
            name,
            credential: credential_value,
            external_id,
            weight,
            max_concurrency,
            quota_group,
            enabled,
            metadata,
        };

        if has_credential {
            self.storage.create_account(&self.secret_box, &new).await
        } else {
            self.storage.accounts().insert(&new).await
        }
    }

    pub async fn find_by_external(
        &self,
        kind: ProviderKind,
        region: Region,
        external_id: &str,
    ) -> Result<Option<Account>, GatewayError> {
        let Some(id) = self
            .storage
            .accounts()
            .find_by_external(kind, region, external_id)
            .await?
        else {
            return Ok(None);
        };
        self.get(&id).await
    }

    pub async fn get(&self, account_id: &str) -> Result<Option<Account>, GatewayError> {
        self.storage.accounts().get(account_id).await
    }
    pub async fn list(&self) -> Result<Vec<Account>, GatewayError> {
        self.storage.accounts().list().await
    }

    /// Every account for one provider kind/region, including disabled and
    /// auth-rejected accounts. Used by the admin surface.
    pub async fn list_by_provider(
        &self,
        kind: ProviderKind,
        region: Region,
    ) -> Result<Vec<Account>, GatewayError> {
        Ok(self
            .storage
            .accounts()
            .list()
            .await?
            .into_iter()
            .filter(|account| account.kind == kind && account.region == region)
            .collect())
    }

    /// Accounts that may be offered to the scheduler/check-in task: enabled,
    /// auth status dispatchable, and actually holding a credential. Unknown
    /// auth is included (a freshly imported account has not been probed yet);
    /// Expired/RefreshRequired/Invalid/Restricted are not.
    pub async fn list_available(
        &self,
        kind: ProviderKind,
        region: Region,
    ) -> Result<Vec<Account>, GatewayError> {
        let mut available = Vec::new();
        for account in self.list_by_provider(kind, region).await? {
            if !account.enabled || !account.auth_status.is_dispatchable() {
                continue;
            }
            let present = self
                .credentials
                .status(&account.id)
                .await?
                .is_some_and(|status| status.present);
            if present {
                available.push(account);
            }
        }
        Ok(available)
    }

    /// Update only mutable metadata. Identity fields are immutable by design.
    pub async fn update(
        &self,
        account_id: &str,
        update: AccountUpdate,
    ) -> Result<Account, GatewayError> {
        let mut account = self
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::new(ErrorKind::BadRequest, "account not found"))?;

        if let Some(name) = update.name {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "account name must not be empty",
                ));
            }
            account.name = name;
        }
        if let Some(weight) = update.weight {
            if weight == 0 {
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "account weight must be at least 1",
                ));
            }
            account.weight = weight;
        }
        if let Some(max_concurrency) = update.max_concurrency {
            if max_concurrency == 0 {
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "account max_concurrency must be at least 1",
                ));
            }
            account.max_concurrency = max_concurrency;
        }
        if let Some(quota_group) = update.quota_group {
            account.quota_group = quota_group;
        }
        if let Some(metadata) = update.metadata {
            account.metadata = metadata;
        }

        if !self.storage.accounts().update(&account).await? {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account not found",
            ));
        }
        self.get(account_id)
            .await?
            .ok_or_else(|| GatewayError::internal("account disappeared after update"))
    }

    pub async fn set_enabled(&self, account_id: &str, enabled: bool) -> Result<bool, GatewayError> {
        self.storage
            .accounts()
            .set_enabled(account_id, enabled)
            .await
    }

    /// Delete the account. Credentials/sessions/quota rows cascade through the
    /// existing foreign keys. Returns `false` when the account did not exist.
    pub async fn delete(&self, account_id: &str) -> Result<bool, GatewayError> {
        self.storage.accounts().delete(account_id).await
    }

    pub async fn auth_status(&self, account_id: &str) -> Result<Option<AuthStatus>, GatewayError> {
        Ok(self
            .get(account_id)
            .await?
            .map(|account| account.auth_status))
    }

    pub async fn is_enabled(&self, account_id: &str) -> Result<Option<bool>, GatewayError> {
        Ok(self.get(account_id).await?.map(|account| account.enabled))
    }

    /// Persist an explicit auth transition (for example after a successful
    /// login/probe). Invalid transitions are rejected.
    pub async fn record_auth_status(
        &self,
        account_id: &str,
        status: AuthStatus,
        last_error: Option<&str>,
    ) -> Result<AuthStatus, GatewayError> {
        self.credentials
            .set_auth_status(account_id, status, last_error)
            .await
    }

    /// Classify a request/probe error and update auth status only when the error
    /// is actually about the credential. Timeouts, transport failures, upstream
    /// 5xx, rate limits and quota exhaustion leave the status untouched.
    pub async fn mark_auth_failure(
        &self,
        account_id: &str,
        error: &GatewayError,
    ) -> Result<AuthStatus, GatewayError> {
        let account = self
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::new(ErrorKind::BadRequest, "account not found"))?;
        let Some(target) = auth_status_for_error(error.kind) else {
            return Ok(account.auth_status);
        };
        if !account.auth_status.can_transition_to(target) {
            return Ok(account.auth_status);
        }
        self.record_auth_status(account_id, target, Some(&error.message))
            .await
    }

    /// Full authorization context for one account. The secret is decrypted
    /// only here and only for the requested account id.
    pub async fn authorization_context(
        &self,
        account_id: &str,
    ) -> Result<AuthorizationContext, GatewayError> {
        let account = self
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::new(ErrorKind::BadRequest, "account not found"))?;
        self.build_context(account).await
    }

    /// Same as [`Self::authorization_context`], but rejects a provider/region
    /// mismatch before decrypting anything. This is the guard that stops a CN
    /// account from being used for an International request (and vice versa).
    pub async fn authorization_context_for(
        &self,
        account_id: &str,
        kind: ProviderKind,
        region: Region,
    ) -> Result<AuthorizationContext, GatewayError> {
        let account = self
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::new(ErrorKind::BadRequest, "account not found"))?;
        if account.kind != kind || account.region != region {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "account provider/region does not match the requested route",
            ));
        }
        self.build_context(account).await
    }

    async fn build_context(&self, account: Account) -> Result<AuthorizationContext, GatewayError> {
        let status: Option<CredentialStatus> = self.credentials.status(&account.id).await?;
        let credential = self.credentials.load(&account.id).await?;
        Ok(AuthorizationContext {
            account_id: account.id,
            provider: account.provider,
            kind: account.kind,
            region: account.region,
            enabled: account.enabled,
            auth_status: account.auth_status,
            credential,
            credential_revision: status.and_then(|value| value.revision),
        })
    }
}

fn auth_status_for_error(kind: ErrorKind) -> Option<AuthStatus> {
    match kind {
        ErrorKind::AuthInvalid => Some(AuthStatus::Invalid),
        ErrorKind::AuthExpired => Some(AuthStatus::Expired),
        // An upstream account/channel restriction is not proof that the secret
        // itself is dead. Keep the credential for a later re-check.
        ErrorKind::AccountDisabled | ErrorKind::ChannelRestricted => Some(AuthStatus::Restricted),
        _ => None,
    }
}
