//! [`CredentialStore`] adapter over [`CredentialService`].
//!
//! Provider adapters that rotate credentials (CodeArts STS) need revision
//! compare-and-swap persistence without linking against storage or this
//! service. The adapter is the only place where that bridge lives.

use async_trait::async_trait;

use crate::accounts::Credential;
use crate::error::GatewayError;
use crate::providers::CredentialStore;

use super::CredentialService;

#[derive(Clone)]
pub struct AccountCredentialStore {
    credentials: CredentialService,
}

impl AccountCredentialStore {
    pub fn new(credentials: CredentialService) -> Self {
        Self { credentials }
    }
}

#[async_trait]
impl CredentialStore for AccountCredentialStore {
    async fn update_credential(
        &self,
        account_id: &str,
        expected_revision: i64,
        credential: &Credential,
    ) -> Result<i64, GatewayError> {
        let status = self
            .credentials
            .update(account_id, credential, expected_revision)
            .await?;
        Ok(status.revision.unwrap_or(expected_revision))
    }

    async fn load_credential(
        &self,
        account_id: &str,
    ) -> Result<Option<(Credential, i64)>, GatewayError> {
        let Some(status) = self.credentials.status(account_id).await? else {
            return Ok(None);
        };
        let Some(revision) = status.revision else {
            return Ok(None);
        };
        let Some(credential) = self.credentials.load(account_id).await? else {
            return Ok(None);
        };
        Ok(Some((credential, revision)))
    }

    async fn acquire_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<bool, GatewayError> {
        self.credentials
            .acquire_refresh_lease(account_id, owner, now, ttl_seconds)
            .await
    }

    async fn release_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
    ) -> Result<(), GatewayError> {
        self.credentials
            .release_refresh_lease(account_id, owner)
            .await
    }
}
