//! OfficeAce (Huawei AgentArts) endpoints and protocol constants.
//!
//! Evidence: Coding2API `src/provider/officeace/events.py` and
//! `docs/OFFICEACE_MIGRATION.md` (protocol facts from the official desktop
//! client's traffic; no code copied). OfficeAce has one deployment
//! (`cn-southwest-2`); an Intl region is rejected at startup.

use crate::config::Region;

/// Control plane: OAuth helpers under `/v1/claw/*`.
pub const CLOUD_HOST: &str = "https://agentarts.cn-southwest-2.myhuaweicloud.com";
/// Fallback model gateway. A real credential carries its own host, which is
/// issued by `client-permission-validate` and differs per account.
pub const DEFAULT_GATEWAY_HOST: &str =
    "https://modelgw-0004.officeace.cn-southwest-2.huaweicloud-agentarts.com";
/// Huawei STS, same host as CodeArts but a different `client_id`.
pub const STS_HOST: &str = "https://sts.cn-north-4.myhuaweicloud.com";
/// Huawei unified login page (PKCE entry point).
pub const AUTH_HOST: &str = "https://auth.huaweicloud.com";

/// Bound to the refresh token; not interchangeable with `codearts-agent`.
pub const CLIENT_ID: &str = "pdp5_for_agentarts";
/// Credential scope service name for `V11-HMAC-SHA256` (APIG always uses `apic`).
pub const SIGN_REGION: &str = "cn-southwest-2";

pub const EP_AUTH_STATE: &str = "/v1/claw/auth/state";
pub const EP_AUTH_CODE: &str = "/v1/claw/auth/code";
pub const EP_CLIENT_PERMISSION: &str = "/v1/claw/client-permission-validate";
pub const EP_AUTHORIZE: &str = "/authui/v1/oauth2/authorize";
pub const EP_LOGIN_PAGE: &str = "/authui/login.html";
pub const EP_LOGOUT: &str = "/authui/logout";
pub const EP_OAUTH_TOKENS: &str = "/v1/oauth2/tokens";
pub const EP_REDIRECT_PATH: &str = "/v1/claw/auth/callback";

pub const EP_SUBSCRIPTION: &str = "/v1/subscription";
pub const EP_BONUS_CLAIM: &str = "/v1/subscription/bonus/claim";
pub const EP_CHAT: &str = "/v2/chat/completions";
pub const EP_MODELS: &str = "/v2/models";

/// SKU attribute that carries the credit balance (points are the main ledger).
pub const POINTS_SKU: &str = "officeace_points";
/// The daily activity window is defined in UTC+8.
pub const BEIJING_OFFSET_SECONDS: i64 = 8 * 3600;
/// Suggested client poll interval for the authorization-code endpoint.
pub const AUTH_POLL_INTERVAL_SECONDS: u64 = 3;
/// STS temporary credentials live about two hours.
pub const TOKEN_TTL_SECONDS: i64 = 2 * 3600;

/// Upper `max_tokens` bound per model family (measured by the reference
/// implementation; the upstream treats `max_tokens` as reasoning + answer).
pub const DEFAULT_MAX_TOKENS: u64 = 393_216;
/// Extra budget for hidden reasoning: nine of eleven usable models reason first,
/// and a small cap yields an empty answer.
pub const REASONING_HEADROOM: u64 = 1024;
/// Values below this are exact-size probes and are forwarded unchanged.
pub const TINY_MAX_TOKENS: u64 = 16;

pub fn supported(region: Region) -> bool {
    region == Region::Cn
}

pub fn max_tokens_cap(model: &str) -> u64 {
    let lowered = model.to_ascii_lowercase();
    if lowered.starts_with("glm-5") {
        131_072
    } else if lowered.starts_with("qwen3") {
        65_536
    } else if lowered.starts_with("deepseek-v4.1-flash") {
        393_216
    } else {
        DEFAULT_MAX_TOKENS
    }
}

/// Client-requested `max_tokens` → the value sent upstream.
pub fn clamp_max_tokens(value: u64, model: &str) -> u64 {
    if value < TINY_MAX_TOKENS {
        return value;
    }
    value
        .saturating_add(REASONING_HEADROOM)
        .min(max_tokens_cap(model))
}

/// Strip the `/v2` suffix some import paths include, so endpoint paths do not
/// become `/v2/v2/...` (observed 404 in the reference implementation).
pub fn normalize_gateway(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v2")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_chinese_deployment_exists() {
        assert!(supported(Region::Cn));
        assert!(!supported(Region::Intl));
    }

    #[test]
    fn a_gateway_with_the_v2_suffix_is_normalized_once() {
        assert_eq!(
            normalize_gateway("https://gw.example/"),
            "https://gw.example"
        );
        assert_eq!(
            normalize_gateway("https://gw.example/v2"),
            "https://gw.example"
        );
        assert_eq!(
            normalize_gateway("https://gw.example/v2/"),
            "https://gw.example"
        );
    }

    #[test]
    fn small_caps_are_unchanged_and_large_ones_get_reasoning_headroom() {
        assert_eq!(clamp_max_tokens(8, "glm-5.2"), 8);
        assert_eq!(clamp_max_tokens(100, "glm-5.2"), 1124);
        assert_eq!(clamp_max_tokens(200_000, "glm-5.2"), 131_072);
    }
}
