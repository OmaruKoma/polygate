//! TRAE region configuration (CN and International).
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §2 and the sources it cites:
//! `Coding2API/src/provider/trae/events.py:29-44` (CN hosts) and
//! `dsh-wtq-pool/src/trae-upstream.ts:47-58` (CN + AI gateways). The two
//! regions are separate account systems; a token issued for one is never sent
//! to the other.
//!
//! The official client identifies itself with a static header family
//! (`X-App-Id`, IDE version, device brand/OS, `Trae/<version>` UA). This adapter
//! does not impersonate that client; the compatibility gaps are listed in the
//! migration document.

use std::fmt;

use crate::config::Region;

pub const CHAT_PATH: &str = "/api/agent/v3/llm_utils_chat";
pub const MODELS_PATH: &str = "/api/ide/v1/get_detail_param";
pub const OAUTH_EXCHANGE_PATH: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";
pub const OAUTH_USER_INFO_PATH: &str = "/cloudide/api/v3/trae/GetUserInfo";
pub const USAGE_PATH: &str = "/trae/api/v2/pay/ide_user_ent_usage";
pub const PAY_STATUS_PATH: &str = "/trae/api/v1/pay/ide_user_pay_status";
pub const CHECKIN_STATUS_PATH: &str = "/trae/api/v2/ug/checkin_credits/status";
pub const CHECKIN_CLAIM_PATH: &str = "/trae/api/v2/ug/checkin_credits/claim";
pub const DEFAULT_FUNCTION: &str = "solo_work_lite";

#[derive(Debug)]
pub struct TraeRealm {
    pub region: Region,
    /// `llm_utils_chat` and model-directory base.
    pub chat_base: &'static str,
    /// Pay/usage base; used from M14-E.
    pub pay_base: &'static str,
    /// OAuth exchange base; empty when the reference has no verified host.
    pub oauth_base: &'static str,
    pub default_domain: &'static str,
    pub domain_suffixes: &'static [&'static str],
}

pub static CN: TraeRealm = TraeRealm {
    region: Region::Cn,
    chat_base: "https://trae-api-cn.mchost.guru",
    pay_base: "https://api.trae.cn",
    oauth_base: "https://api.trae.com.cn",
    default_domain: "trae.cn",
    domain_suffixes: &["trae.cn", "trae.com.cn"],
};

pub static INTL: TraeRealm = TraeRealm {
    region: Region::Intl,
    chat_base: "https://coresg-normal.trae.ai",
    pay_base: "https://growsg-normal.trae.ai",
    // No verified International ExchangeToken host in either reference.
    oauth_base: "",
    default_domain: "trae.ai",
    domain_suffixes: &["trae.ai"],
};

/// Realm lookup. There is intentionally no "unknown -> CN" fallback.
pub fn realm(region: Region) -> &'static TraeRealm {
    match region {
        Region::Cn => &CN,
        Region::Intl => &INTL,
    }
}

impl TraeRealm {
    pub fn chat_url(&self, base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), CHAT_PATH)
    }

    pub fn models_url(&self, base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), MODELS_PATH)
    }

    /// The credential `domain` must belong to the account's region.
    pub fn accepts_domain(&self, domain: &str) -> bool {
        let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
        if domain.is_empty() {
            return true;
        }
        self.domain_suffixes
            .iter()
            .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
    }

    /// `apiHost` is operator-supplied, so it is validated before it is ever
    /// used as a request base. Only **https** hosts inside the realm pass:
    /// the documented gateway hosts (`trae-api-cn.mchost.guru`,
    /// `coresg-normal.trae.ai`, …) or a `trae.*` domain. An http host would put
    /// the refresh token on the wire in clear text in M14-E.
    pub fn accepts_api_host(&self, host: &str) -> bool {
        let host = host.trim();
        if host.is_empty() {
            return true;
        }
        let Ok(url) = url::Url::parse(host) else {
            return false;
        };
        if url.scheme() != "https" {
            return false;
        }
        let Some(hostname) = url.host_str() else {
            return false;
        };
        let official = [self.chat_base, self.pay_base, self.oauth_base]
            .iter()
            .filter_map(|base| url::Url::parse(base).ok())
            .filter_map(|base| base.host_str().map(str::to_string))
            .any(|known| known == hostname);
        official || self.accepts_domain(hostname)
    }
}

impl fmt::Display for TraeRealm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TRAE {}", self.region.as_str().to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realms_never_share_hosts_or_domains() {
        assert_ne!(CN.chat_base, INTL.chat_base);
        assert_ne!(CN.pay_base, INTL.pay_base);
        assert_eq!(CN.default_domain, "trae.cn");
        assert_eq!(INTL.default_domain, "trae.ai");
        assert!(INTL.oauth_base.is_empty());
    }

    #[test]
    fn domains_belong_to_one_realm_only() {
        assert!(CN.accepts_domain("trae.cn"));
        assert!(CN.accepts_domain("api.trae.com.cn"));
        assert!(!CN.accepts_domain("api.trae.ai"));
        assert!(INTL.accepts_domain("trae.ai"));
        assert!(!INTL.accepts_domain("www.trae.cn"));
        assert!(CN.accepts_domain(""));
    }

    #[test]
    fn api_hosts_are_validated() {
        assert!(CN.accepts_api_host("https://api.trae.com.cn/"));
        assert!(CN.accepts_api_host("https://trae-api-cn.mchost.guru"));
        assert!(INTL.accepts_api_host("https://coresg-normal.trae.ai"));
        assert!(!CN.accepts_api_host("https://evil.example.com"));
        assert!(!CN.accepts_api_host("http://api.trae.com.cn"));
        assert!(!CN.accepts_api_host("file:///etc/passwd"));
        assert!(!INTL.accepts_api_host("https://trae-api-cn.mchost.guru"));
        assert!(CN.accepts_api_host(""));
    }

    #[test]
    fn urls_keep_the_configured_base() {
        assert_eq!(
            CN.chat_url("http://127.0.0.1:9/"),
            "http://127.0.0.1:9/api/agent/v3/llm_utils_chat"
        );
        assert_eq!(
            INTL.models_url("http://127.0.0.1:9"),
            "http://127.0.0.1:9/api/ide/v1/get_detail_param"
        );
    }
}
