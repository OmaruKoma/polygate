//! WorkBuddy realm configuration (CN and International).
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §2 and §5. The two realms are
//! separate account systems with separate domains; a token issued for one is
//! never sent to the other.
//!
//! Source: `workbuddy2api/admin/backend.py:119-120` (CN chat/billing hosts),
//! `dsh/src/upstream.ts:174-215` (CN host and Intl hosts),
//! `Coding2API/src/provider/codebuddy/headers.py:16-17` (hosts) — MIT, protocol
//! facts only.

use crate::config::Region;
use crate::error::GatewayError;

pub const CHAT_PATH: &str = "/v2/chat/completions";
pub const CONFIG_PATH: &str = "/v3/config";
pub const AUTH_STATE_PATH: &str = "/v2/plugin/auth/state";
pub const AUTH_TOKEN_PATH: &str = "/v2/plugin/auth/token";
pub const LOGIN_ACCOUNT_PATH: &str = "/v2/plugin/login/account";
pub const REFRESH_PATH: &str = "/v2/plugin/auth/token/refresh";
pub const USER_RESOURCE_PATH: &str = "/v2/billing/meter/get-user-resource";
pub const CHECKIN_STATUS_PATH: &str = "/v2/billing/meter/checkin-activity-status";
pub const CHECKIN_CLAIM_PATH: &str = "/v2/billing/meter/daily-checkin";
const ENTERPRISE_MODELS_PREFIX: &str = "/v2/enterprises/";

#[derive(Debug)]
pub struct WorkbuddyRealm {
    pub region: Region,
    pub chat_base: &'static str,
    pub billing_base: &'static str,
    pub default_domain: &'static str,
    /// Intl refuses a request whose first message is not a system prompt
    /// (`first message is not system prompt`). The adapter does not inject one;
    /// the flag exists so the limitation is visible, not silently patched.
    pub requires_leading_system: bool,
    pub domain_suffixes: &'static [&'static str],
}

pub static CN: WorkbuddyRealm = WorkbuddyRealm {
    region: Region::Cn,
    chat_base: "https://copilot.tencent.com",
    billing_base: "https://www.codebuddy.cn",
    default_domain: "www.codebuddy.cn",
    requires_leading_system: false,
    domain_suffixes: &["codebuddy.cn", "workbuddy.cn"],
};

pub static INTL: WorkbuddyRealm = WorkbuddyRealm {
    region: Region::Intl,
    chat_base: "https://www.workbuddy.ai",
    billing_base: "https://www.workbuddy.ai",
    default_domain: "www.workbuddy.ai",
    requires_leading_system: true,
    domain_suffixes: &["workbuddy.ai", "codebuddy.ai", "workbuddy.cc"],
};

/// Realm lookup. There is intentionally no "unknown -> CN" fallback.
pub fn realm(region: Region) -> &'static WorkbuddyRealm {
    match region {
        Region::Cn => &CN,
        Region::Intl => &INTL,
    }
}

impl WorkbuddyRealm {
    pub fn chat_url(&self, base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), CHAT_PATH)
    }

    pub fn url_at(&self, base: &str, path: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), path)
    }

    /// CN scopes the catalogue by enterprise, `personal` when the account has
    /// none; Intl serves the whole catalogue from `/v3/config`.
    pub fn models_path(&self, enterprise_id: &str) -> Result<String, GatewayError> {
        match self.region {
            Region::Intl => Ok(CONFIG_PATH.to_string()),
            Region::Cn => {
                let scope = enterprise_id.trim();
                if scope.is_empty() {
                    return Ok(format!("{ENTERPRISE_MODELS_PREFIX}personal/models"));
                }
                if !scope
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
                {
                    return Err(GatewayError::auth_invalid(
                        "WorkBuddy enterprise id is not a simple identifier",
                    ));
                }
                Ok(format!("{ENTERPRISE_MODELS_PREFIX}{scope}/models"))
            }
        }
    }

    pub fn accepts_domain(&self, domain: &str) -> bool {
        let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return true;
        }
        self.domain_suffixes
            .iter()
            .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realms_never_share_hosts_or_domains() {
        assert_ne!(CN.chat_base, INTL.chat_base);
        assert_ne!(CN.default_domain, INTL.default_domain);
        assert!(!CN.requires_leading_system);
        assert!(INTL.requires_leading_system);
    }

    #[test]
    fn models_path_is_region_specific() {
        assert_eq!(
            CN.models_path("").unwrap(),
            "/v2/enterprises/personal/models"
        );
        assert_eq!(
            CN.models_path("ent-1").unwrap(),
            "/v2/enterprises/ent-1/models"
        );
        assert_eq!(INTL.models_path("ent-1").unwrap(), "/v3/config");
        assert!(CN.models_path("../../etc").is_err());
    }

    #[test]
    fn a_token_domain_belongs_to_one_realm_only() {
        assert!(CN.accepts_domain("www.codebuddy.cn"));
        assert!(!CN.accepts_domain("www.workbuddy.ai"));
        assert!(INTL.accepts_domain("www.workbuddy.ai"));
        assert!(INTL.accepts_domain("codebuddy.ai"));
        assert!(!INTL.accepts_domain("www.codebuddy.cn"));
        assert!(CN.accepts_domain(""));
    }

    #[test]
    fn chat_url_keeps_the_configured_base() {
        assert_eq!(
            CN.chat_url("http://127.0.0.1:9/"),
            "http://127.0.0.1:9/v2/chat/completions"
        );
    }
}
