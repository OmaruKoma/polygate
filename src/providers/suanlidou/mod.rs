//! China Mobile "Suanlidou" (算力豆) provider — scaffold.
//!
//! Status: the upstream protocol has not been captured yet, so this provider
//! deliberately implements **no capability**. It exists so a provider entry and
//! its accounts can be created ahead of the capture; every probe answers
//! `Unsupported` instead of inventing an endpoint or a balance.
//!
//! Known scope (from the operator): there is no check-in on this platform, only
//! a remaining-quota query. Check-in is therefore absent from the routing table,
//! and the quota probe stays unsupported until a real capture exists.

use async_trait::async_trait;

use crate::accounts::Credential;
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::providers::{Capabilities, Provider, ProviderInfo};
use crate::quota::QuotaSnapshot;

const NOT_CAPTURED: &str =
    "Suanlidou protocol has not been captured yet; only the quota probe is planned";

pub struct SuanlidouProvider {
    info: ProviderInfo,
}

impl std::fmt::Debug for SuanlidouProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuanlidouProvider")
            .field("id", &self.info.id)
            .finish_non_exhaustive()
    }
}

impl SuanlidouProvider {
    pub fn new(id: impl Into<String>, region: Region, verification: Verification) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Suanlidou,
            region,
            display_name: ProviderKind::Suanlidou.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self { info }
    }

    fn not_captured() -> GatewayError {
        GatewayError::new(ErrorKind::Unsupported, NOT_CAPTURED)
    }
}

#[async_trait]
impl Provider for SuanlidouProvider {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            authentication: false,
            model_discovery: false,
            quota: false,
            chat: false,
            streaming: false,
            tools: false,
            native_session: false,
            agent_task: false,
            cancellation: false,
        }
    }

    async fn quota(&self, _credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        // The only planned capability; refusing now keeps `unknown` honest.
        Err(Self::not_captured())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;

    fn provider() -> SuanlidouProvider {
        SuanlidouProvider::new("suanlidou-cn", Region::Cn, Verification::Unavailable)
    }

    #[test]
    fn declares_no_capability_until_the_protocol_is_captured() {
        let capabilities = provider().capabilities();
        assert!(!capabilities.authentication);
        assert!(!capabilities.model_discovery);
        assert!(!capabilities.quota);
        assert!(!capabilities.chat);
        assert!(!capabilities.streaming);
    }

    #[test]
    fn identity_is_registered_under_its_own_kind() {
        let info = provider().info();
        assert_eq!(info.kind, ProviderKind::Suanlidou);
        assert_eq!(info.id, "suanlidou-cn");
        assert_eq!(info.region, Region::Cn);
        assert_eq!(
            ProviderKind::from_db_str("suanlidou"),
            Some(ProviderKind::Suanlidou)
        );
    }

    #[tokio::test]
    async fn quota_refuses_instead_of_reporting_a_zero_balance() {
        let error = provider()
            .quota(&Credential::from_value(serde_json::json!({})))
            .await
            .expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(
            error.message.contains("not been captured"),
            "{}",
            error.message
        );
    }
}
