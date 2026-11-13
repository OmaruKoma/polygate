//! Doubao (豆包 / DoubaoWork) deployment constants.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md`, derived from the reference gateway
//! `2951461586/Doubao2API` (Apache-2.0). Only protocol facts were reused; no
//! reference code was copied.
//!
//! There is exactly one deployment (`www.doubao.com`); an `intl` entry is a
//! configuration error, not a silently accepted alias.

use crate::config::Region;

pub const CHAT_BASE: &str = "https://www.doubao.com";
pub const CHAT_PATH: &str = "/chat/completion";
pub const PASSPORT_BASE: &str = "https://accounts.doubao.com";

/// Main-site app id. The desktop build uses `1044603`; the web pipeline the
/// upstream accepts for `device_platform=web` is `582478`.
pub const APP_ID: &str = "582478";
pub const VERSION_CODE: &str = "20800";
pub const CLIENT_PLATFORM: &str = "pc_client";
pub const RUNTIME_VERSION: &str = "3.5.4";
pub const PC_VERSION: &str = "2.1.7";
pub const CHROMIUM_VERSION: &str = "147.0.7727.149";

/// Default bot: the desktop "extension bot" that can serve every content block.
pub const QUOTA_PATH: &str = "/alice/commerce/sale/subscription/quota/summary/";
pub const QUOTA_PRODUCT_LINE: &str = "doubao";
pub const JSON_TIMEOUT_SECONDS: u64 = 30;

pub const DEFAULT_BOT_ID: &str = "7338286299411103781";

/// `option.need_deep_think` / `ext.use_deep_think` tiers.
pub const THINK_OFF: i64 = 0;
pub const THINK_ON: i64 = 1;
pub const THINK_EXPERT: i64 = 3;

pub const REQUEST_TIMEOUT_SECONDS: u64 = 120;
pub const STREAM_IDLE_SECONDS: u64 = 120;
const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;

pub fn max_json_body() -> usize {
    MAX_JSON_BODY_BYTES
}

pub fn supported(region: Region) -> bool {
    region == Region::Cn
}

/// Public model id -> thinking tier. The upstream does not route by model name
/// at all: the tier is the only dial the client exposes.
pub fn tier_for_model(model: &str) -> Option<i64> {
    match model.trim().to_ascii_lowercase().as_str() {
        "doubao" | "doubao-fast" | "doubao-lite" | "doubao-pro" => Some(THINK_OFF),
        "doubao-think" | "doubao-thinking" | "doubao-reason" => Some(THINK_ON),
        "doubao-expert" | "doubao-deep" | "doubao-max" => Some(THINK_EXPERT),
        _ => None,
    }
}

/// Catalogue advertised through `/v1/models`. Context sizes are the ones the
/// reference project documents; the upstream itself does not report them.
pub fn catalog() -> Vec<(&'static str, &'static str, u64)> {
    let mut models = vec![
        ("doubao", "Doubao · Fast", 128_000),
        ("doubao-think", "Doubao · Think", 128_000),
        ("doubao-expert", "Doubao · Expert", 256_000),
    ];
    for model in WORK_MODELS {
        models.push((model.id, model.label, model.window));
    }
    models
}

/// A third-party model the desktop client's "工作" menu exposes on the same
/// account. The menu is server-driven: `option.need_deep_think` carries the
/// menu type and `model_item_key` names the model (captured from the client on
/// 2026-11-13, see `docs/DOUBAO_MIGRATION.md` §12).
pub struct WorkModel {
    /// Public model id used by this gateway.
    pub id: &'static str,
    /// `model_item_key` the client sends.
    pub key: &'static str,
    pub label: &'static str,
    pub window: u64,
}

/// `need_deep_think` / `use_deep_think` value that selects the work lane.
pub const WORK_THINK: i64 = 10_001;

pub const WORK_MODELS: &[WorkModel] = &[
    WorkModel {
        id: "doubao-deepseek-v4.1-flash",
        key: "person-deepseek-v4-1-flash-261017",
        label: "DeepSeek V4.1 Flash · 豆包工作（实验性）",
        window: 300_000,
    },
    WorkModel {
        id: "doubao-glm-5.3",
        key: "person-glm-5-3-261111",
        label: "GLM 5.3 · 豆包工作（实验性）",
        window: 300_000,
    },
    WorkModel {
        id: "doubao-glm-5.3-flash",
        key: "person-glm-5-3-flash-261111",
        label: "GLM 5.3 Flash · 豆包工作（实验性）",
        window: 300_000,
    },
    WorkModel {
        id: "doubao-kimi-k3-pro",
        key: "person-kimi-k3-1-pro-261111",
        label: "Kimi K3 Pro · 豆包工作（实验性）",
        window: 300_000,
    },
    WorkModel {
        id: "doubao-kimi-k3-pro-highspeed",
        key: "person-kimi-k3-1-pro-highspeed-261111",
        label: "Kimi K3 Pro Highspeed · 豆包工作（实验性）",
        window: 300_000,
    },
];

pub fn work_model(model: &str) -> Option<&'static WorkModel> {
    let name = model.trim().to_ascii_lowercase();
    WORK_MODELS.iter().find(|entry| entry.id == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_are_resolved_from_the_public_names() {
        assert_eq!(tier_for_model("doubao"), Some(THINK_OFF));
        assert_eq!(tier_for_model("Doubao-Think"), Some(THINK_ON));
        assert_eq!(tier_for_model("doubao-expert"), Some(THINK_EXPERT));
        assert_eq!(tier_for_model("gpt-4o"), None);
    }

    #[test]
    fn work_models_resolve_to_their_menu_keys() {
        assert_eq!(
            work_model("doubao-glm-5.3").map(|m| m.key),
            Some("person-glm-5-3-261111")
        );
        assert_eq!(
            work_model("Doubao-Kimi-K3-Pro").map(|m| m.key),
            Some("person-kimi-k3-1-pro-261111")
        );
        assert!(work_model("doubao").is_none());
    }

    #[test]
    fn the_catalogue_lists_both_lanes() {
        let models = catalog();
        assert!(models.iter().any(|(id, _, _)| *id == "doubao"));
        assert!(models.iter().any(|(id, _, _)| *id == "doubao-kimi-k3-pro"));
    }

    #[test]
    fn only_the_chinese_deployment_exists() {
        assert!(supported(Region::Cn));
        assert!(!supported(Region::Intl));
    }
}
