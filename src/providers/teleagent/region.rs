//! TeleAgent (China Telecom 星辰超级智能体) deployment constants.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md`, reverse engineered from the desktop
//! client 3.1.0 (`C:\Program Files\TeleAgent`) on 2026-11-18: the Go engine
//! signs every model call with a two-step HMAC (`GatewaySigner.Sign`), and the
//! request/response shape is OpenAI-compatible.
//!
//! There is one deployment (`agent.teleai.com.cn`); a CN login is never valid
//! anywhere else, so `Region::Intl` is rejected at registry build time.

use crate::config::Region;

pub const BASE: &str = "https://agent.teleai.com.cn";
pub const CHAT_PATH: &str = "/superCowork/sapi/api/v1/chat/completions";
pub const MODEL_OPTIONS_PATH: &str = "/superCowork/sapi/api/v1/model-options";
pub const USER_CENTER_PATH: &str = "/superCowork/sapi/user/portal/companyStore/userCenter";

/// Welfare page: lists the daily-claim card with its state (`incomplete` /
/// `completed`) and the claim URL.
pub const DAY_TASK_PATH: &str = "/superCowork/sapi/user/portal/companyStore/v3/checkDayTask";
/// One-time task list (`first_login`, `monthly_basic`, …). Empty once claimed.
pub const CLAIMABLE_TASKS_PATH: &str = "/superCowork/sapi/user/portal/companyStore/checkTask";
/// Claiming the daily 100 points. Verified live on 2026-11-19: balance
/// 9096.17 → 9196.15 and the card flipped to `completed`.
pub const DAILY_CLAIM_PATH: &str =
    "/superCowork/sapi/user/portal/companyStore/checkTask?type=daily_login";

/// Product-wide constant shipped inside the client. It identifies "this is a
/// TeleAgent client"; the account identity rides on `X-Token`.
pub const CLIENT_API_KEY: &str = "sk-qvp4m6h9g20uM6PgYP7E";

/// First HMAC segment and the first field of both signed messages.
pub const SIGN_MAGIC: &str = "superagent-auth-v1";
pub const SIGN_VERSION: &str = "v1";

/// Version string the client sends; the server echoes it back.
pub const DEFAULT_APP_VERSION: &str = "3.1.0";
pub const ROUTE_TARGET: &str = "ops-gateway";

/// Documented figures from the client's `model-catalog.json` (2026-11-18).
/// The `/model-options` endpoint reports `context_limit: null` for every entry,
/// so these stay a documented table rather than a discovered value.
pub const CHAT_PRO_CONTEXT: u64 = 400_000;
pub const CHAT_PRO_MAX_OUTPUT: u64 = 64_000;

/// JWT lifetime observed on the desktop token (7 days, no refresh token).
pub const TOKEN_TTL_SECONDS: i64 = 7 * 24 * 3600;

pub const REQUEST_TIMEOUT_SECONDS: u64 = 900;
pub const JSON_TIMEOUT_SECONDS: u64 = 30;
const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;

pub fn max_json_body() -> usize {
    MAX_JSON_BODY_BYTES
}

/// The only supported region.
pub fn supported(region: Region) -> bool {
    region == Region::Cn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_chinese_deployment_exists() {
        assert!(supported(Region::Cn));
        assert!(!supported(Region::Intl));
    }
}
