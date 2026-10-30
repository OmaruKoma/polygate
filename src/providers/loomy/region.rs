//! Loomy (iFlytek) deployment constants.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md` §2 and the reference implementation
//! `Patrick130306/loomy2api` (MIT) `docs/PROTOCOL.md` §1, derived from the
//! official Electron client 0.9.38.
//!
//! There is exactly one deployment. `Region::Cn` is the only accepted region;
//! an `Intl` entry is rejected at registry build time rather than silently
//! sending a CN session to a host that does not exist.

use crate::config::Region;

pub const MODEL_BASE: &str = "https://loomyad.xunfei.cn/api/v1";
pub const MODELS_PATH: &str = "/models";
pub const CHAT_PATH: &str = "/chat/completions";
pub const POINTS_PATH: &str = "/points/records";

/// Presence-checked only by the model gateway.
pub const LOOMY_VERSION: &str = "0.9.38";

/// Documented session lifetime: 14 days, no refresh token.
pub const SESSION_TTL_SECONDS: i64 = 14 * 24 * 3600;

pub const REQUEST_TIMEOUT_SECONDS: u64 = 120;
pub const JSON_TIMEOUT_SECONDS: u64 = 30;
const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;

pub fn max_json_body() -> usize {
    MAX_JSON_BODY_BYTES
}

/// The only supported region.
pub fn supported(region: Region) -> bool {
    region == Region::Cn
}
