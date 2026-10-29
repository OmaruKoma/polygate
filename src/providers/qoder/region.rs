//! Qoder realm configuration (CN and International).
//!
//! Source: Coding2API `src/provider/qoder/events.py` (`REALM_CONFIGS` and the
//! `EP_*` constants) at commit `df9936240708f99f5a1cd92e52330b4b2016c840`.
//! The two realms are independent upstreams: different OpenAPI host, inference
//! gateway, website, cookie domain, client User-Agent and nonce shape.
//!
//! This module deliberately has no fallback: [`realm`] matches [`Region`]
//! exhaustively. A CN request must never be sent to an Intl host and vice
//! versa. Gateway host fallback inside one realm is allowed (the signature only
//! covers the path, not the host) but is a separate decision made by the caller.

use crate::config::Region;
use crate::error::GatewayError;

/// Device-authorization nonce shape. CN uses a dashed UUID, Intl a hex UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceStyle {
    DashedUuid,
    HexUuid,
}

impl NonceStyle {
    pub fn new_nonce(self) -> String {
        let uuid = uuid::Uuid::new_v4();
        match self {
            NonceStyle::DashedUuid => uuid.to_string(),
            NonceStyle::HexUuid => uuid.simple().to_string(),
        }
    }
}

/// OpenAPI business endpoints (Bearer auth; no COSY signature).
pub const DEVICE_POLL_PATH: &str = "/api/v1/deviceToken/poll";
pub const DEVICE_REFRESH_PATH: &str = "/api/v1/deviceToken/refresh";
pub const USERINFO_PATH: &str = "/api/v1/userinfo";
pub const QUOTA_PATH: &str = "/api/v2/quota/usage";
/// Declared by the reference implementation but not parsed there; unverified.
pub const PLAN_PATH: &str = "/api/v2/user/plan";
pub const CAMPAIGNS_PATH: &str = "/sash/api/v1/me/campaigns";
pub const CAMPAIGN_CLAIM_PATH: &str = "/sash/api/v1/me/campaigns/{campaign_id}/claim";
pub const LEGACY_CHECKIN_STATUS_PATH: &str = "/sash/api/v1/me/daily-check-in/status";
pub const LEGACY_CHECKIN_CLAIM_PATH: &str = "/sash/api/v1/me/daily-check-in/claim";

/// Inference gateway endpoints (COSY signature).
pub const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";
pub const CHAT_QUERY: &str = "FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
pub const MODELS_PATH: &str = "/algo/api/v2/model/list?Encode=1";
/// The signature does not cover the HTTP method. These are the only methods the
/// reference implementation uses: a model list GET and a chat POST.
pub const MODELS_METHOD: &str = "GET";
pub const CHAT_METHOD: &str = "POST";

/// One realm's upstream addresses and login parameters.
#[derive(Debug)]
pub struct QoderRealm {
    pub region: Region,
    pub openapi_base: &'static str,
    pub gateway_base: &'static str,
    pub gateway_fallbacks: &'static [&'static str],
    pub website: &'static str,
    pub domain: &'static str,
    /// Client hint from the official desktop app. Not the User-Agent used by
    /// the HTTP/COSY paths (that is the fixed `Go-http-client/2.0` constant).
    pub user_agent: &'static str,
    pub client_id: &'static str,
    pub redirect_uri: &'static str,
    /// The reference implementation sends no `redirect_uri` on either realm;
    /// the switch is kept so an upstream change can be followed explicitly.
    pub send_redirect_uri: bool,
    pub nonce_style: NonceStyle,
}

pub static CN: QoderRealm = QoderRealm {
    region: Region::Cn,
    openapi_base: "https://openapi.qoder.com.cn",
    gateway_base: "https://gateway.qoder.com.cn",
    gateway_fallbacks: &[],
    website: "https://qoder.cn",
    domain: "qoder.com.cn",
    user_agent: "QoderWork/1.1.64",
    client_id: "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
    redirect_uri: "qoder-work-cn://",
    send_redirect_uri: false,
    nonce_style: NonceStyle::DashedUuid,
};

pub static INTL: QoderRealm = QoderRealm {
    region: Region::Intl,
    openapi_base: "https://openapi.qoder.sh",
    gateway_base: "https://api1.qoder.sh",
    gateway_fallbacks: &["https://api2.qoder.sh", "https://api3.qoder.sh"],
    website: "https://qoder.com",
    domain: "qoder.com",
    user_agent: "Qoder/1.1.64",
    client_id: "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
    redirect_uri: "qoder://aicoding.aicoding-agent/login-success",
    send_redirect_uri: false,
    nonce_style: NonceStyle::HexUuid,
};

/// Realm lookup. There is intentionally no "unknown -> CN" fallback.
pub fn realm(region: Region) -> &'static QoderRealm {
    match region {
        Region::Cn => &CN,
        Region::Intl => &INTL,
    }
}

impl QoderRealm {
    fn join(&self, base: &str, path: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), path)
    }

    pub fn openapi(&self, path: &str) -> String {
        self.join(self.openapi_base, path)
    }

    pub fn gateway(&self, path: &str) -> String {
        self.join(self.gateway_base, path)
    }

    pub fn device_poll_url(&self) -> String {
        self.openapi(DEVICE_POLL_PATH)
    }

    pub fn refresh_url(&self) -> String {
        self.openapi(DEVICE_REFRESH_PATH)
    }

    pub fn userinfo_url(&self) -> String {
        self.openapi(USERINFO_PATH)
    }

    pub fn quota_url(&self) -> String {
        self.openapi(QUOTA_PATH)
    }

    pub fn campaigns_url(&self) -> String {
        self.openapi(CAMPAIGNS_PATH)
    }

    pub fn legacy_checkin_status_url(&self) -> String {
        self.openapi(LEGACY_CHECKIN_STATUS_PATH)
    }

    pub fn legacy_checkin_claim_url(&self) -> String {
        self.openapi(LEGACY_CHECKIN_CLAIM_PATH)
    }

    pub fn chat_url(&self) -> String {
        self.gateway(&format!("{CHAT_PATH}?{CHAT_QUERY}"))
    }

    pub fn models_url(&self) -> String {
        self.gateway(MODELS_PATH)
    }

    /// `campaign_id` comes from the upstream campaign list. Reject anything
    /// that is not a simple identifier so it cannot alter the URL path.
    pub fn campaign_claim_url(&self, campaign_id: &str) -> Result<String, GatewayError> {
        if campaign_id.is_empty()
            || !campaign_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            return Err(GatewayError::bad_request("invalid Qoder campaign id"));
        }
        Ok(self.openapi(&CAMPAIGN_CLAIM_PATH.replace("{campaign_id}", campaign_id)))
    }

    /// Primary gateway followed by the same realm's official fallbacks.
    pub fn gateway_candidates(&self) -> Vec<&'static str> {
        let mut hosts = Vec::with_capacity(1 + self.gateway_fallbacks.len());
        hosts.push(self.gateway_base);
        for host in self.gateway_fallbacks {
            if !hosts.contains(host) {
                hosts.push(host);
            }
        }
        hosts
    }

    pub fn new_nonce(&self) -> String {
        self.nonce_style.new_nonce()
    }
}
