//! Official-client identity profiles ("impersonation").
//!
//! PolyGate's adapters originally sent only protocol-required headers plus their
//! own `polygate/<version>` User-Agent. Three upstreams run client-side checks
//! that an honest gateway trips (WorkBuddy answers business code 11128 to a
//! request without a conversation-id family; Qoder and TRAE expect the desktop
//! client's identity headers). This module carries the constants and header
//! builders of the real clients so one switch can turn that identity on.
//!
//! Evidence: `Coding2API/src/provider/{codebuddy/headers,trae/client,qoder/cosy}.py`,
//! `dsh-wtq-pool/src/upstream.ts:520-700`, `workbuddy2api/converter.py:380-412`.
//! Values are protocol facts, not secrets. Version numbers are the ones the
//! captures were taken from and go stale with the clients; they are operator
//! overridable per provider where that matters.
//!
//! Request-scoped ids (conversation, trace, turn) are fresh on every call. The
//! device-shaped values (Qoder `machineid`/`machinetoken`) are derived from the
//! uid instead, because the upstream groups calls by them.

use std::net::IpAddr;

use crate::config::Region;

/// Which identity a provider presents upstream.
///
/// `Honest` is the original behaviour: protocol-required headers and a
/// `polygate/<version>` UA. `Impersonate` adds the official client's header
/// family and UA. It is a configuration switch, not a capability claim — the
/// adapters do not generate device identity either way, they only replay the
/// public client constants and derive per-uid values the upstream itself
/// derives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Identity {
    /// No client impersonation.
    #[default]
    Honest,
    /// Present the desktop client's identity.
    Impersonate,
}

impl Identity {
    pub fn is_impersonating(self) -> bool {
        self == Identity::Impersonate
    }
}

impl std::str::FromStr for Identity {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "honest" | "" => Ok(Identity::Honest),
            "impersonate" | "client" | "official" => Ok(Identity::Impersonate),
            _ => Err(()),
        }
    }
}

// ---------------------------------------------------------------------------
// WorkBuddy / CodeBuddy
// ---------------------------------------------------------------------------

/// CN desktop client version (capture 2026-10-27).
pub const WORKBUDDY_CN_CLIENT_VERSION: &str = "5.9.3";
pub const WORKBUDDY_CN_CLI_VERSION: &str = "2.165.0";
/// International desktop client version.
pub const WORKBUDDY_INTL_CLIENT_VERSION: &str = "5.8.4";
pub const WORKBUDDY_INTL_CLI_VERSION: &str = "2.161.0";
/// Bundled OpenAI Node SDK fingerprint.
pub const WORKBUDDY_STAINLESS_PACKAGE: &str = "6.25.0";
pub const WORKBUDDY_STAINLESS_RUNTIME: &str = "v24.11.1";

pub fn workbuddy_client_version(region: Region) -> &'static str {
    match region {
        Region::Cn => WORKBUDDY_CN_CLIENT_VERSION,
        Region::Intl => WORKBUDDY_INTL_CLIENT_VERSION,
    }
}

fn workbuddy_cli_version(region: Region) -> &'static str {
    match region {
        Region::Cn => WORKBUDDY_CN_CLI_VERSION,
        Region::Intl => WORKBUDDY_INTL_CLI_VERSION,
    }
}

/// UA for the chat path: the desktop client's three-segment spelling. The Intl
/// gateway uses `workbuddy-ai/` as its middle segment.
pub fn workbuddy_chat_ua(region: Region) -> String {
    let version = workbuddy_client_version(region);
    let cli = workbuddy_cli_version(region);
    match region {
        Region::Cn => format!("WorkBuddy/{version} WorkBuddy/{version} CLI/{cli}"),
        Region::Intl => format!("WorkBuddy/{version} workbuddy-ai/{version} CLI/{cli}"),
    }
}

/// UA for every endpoint the desktop process calls outside the chat path. The
/// Intl gateway serves its product config only to this spelling.
pub fn workbuddy_desktop_ua(region: Region) -> String {
    format!("WorkBuddy/{}", workbuddy_client_version(region))
}

fn stainless_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "MacOS"
    } else {
        "Linux"
    }
}

fn stainless_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

/// Both gateways route the caller as a non-client without this pair; the Intl
/// one answers 11128 outright.
pub fn workbuddy_gate_headers() -> Vec<(&'static str, String)> {
    vec![
        ("X-Requested-With", "XMLHttpRequest".into()),
        ("X-CodeBuddy-Request", "1".into()),
        ("X-Product", "SaaS".into()),
    ]
}

pub fn workbuddy_stainless_headers() -> Vec<(&'static str, String)> {
    vec![
        ("x-stainless-arch", stainless_arch().into()),
        ("x-stainless-lang", "js".into()),
        ("x-stainless-os", stainless_os().into()),
        (
            "x-stainless-package-version",
            WORKBUDDY_STAINLESS_PACKAGE.into(),
        ),
        ("x-stainless-retry-count", "0".into()),
        ("x-stainless-runtime", "node".into()),
        (
            "x-stainless-runtime-version",
            WORKBUDDY_STAINLESS_RUNTIME.into(),
        ),
    ]
}

pub fn workbuddy_origin_headers(base: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Origin", base.to_string()),
        ("Referer", format!("{base}/")),
    ]
}

/// Conversation/trace family. Shapes follow the 5.8.4 capture: dashed UUID for
/// the conversation id, 32 hex characters for the rest, and the request/message
/// ids mirrored into the root-request header the client sends.
pub fn workbuddy_conversation_headers(region: Region) -> Vec<(&'static str, String)> {
    let turn = hex_id();
    let message = hex_id();
    let trace = hex_id();
    let span = hex_span();
    let mut headers = vec![
        ("X-Conversation-ID", uuid::Uuid::new_v4().to_string()),
        ("X-Conversation-Request-ID", turn.clone()),
        ("X-Root-Request-ID", turn),
        ("X-Conversation-Message-ID", message.clone()),
        ("X-Request-ID", message),
        ("X-Agent-Intent", "craft".into()),
        ("X-Agent-Purpose", "conversation".into()),
        ("X-Agent-Type", "main".into()),
        (
            "X-IDE-Version",
            workbuddy_client_version(region).to_string(),
        ),
        ("X-IDE-Name", "WorkBuddy".into()),
        ("X-IDE-Type", "WorkBuddy".into()),
        ("X-Private-Data", "true".into()),
        ("X-Trace-ID", trace.clone()),
        ("traceparent", format!("00-{trace}-{span}-01")),
        ("b3", format!("{trace}-{span}-1")),
        ("X-B3-TraceId", trace),
        ("X-B3-SpanId", span),
        ("X-B3-Sampled", "1".into()),
    ];
    // CN 5.9 names its agent runtime; the international build does not.
    if region == Region::Cn {
        headers.push(("X-Agent-Runtime", "local-agent".into()));
    }
    headers
}

/// Full chat identity: gate, origin, SDK fingerprint, conversation ids, plus the
/// `X-No-*` markers for fields this account cannot fill.
pub fn workbuddy_chat_headers(
    region: Region,
    base: &str,
    uid: &str,
    enterprise_id: &str,
    domain: &str,
) -> Vec<(&'static str, String)> {
    let mut headers = workbuddy_gate_headers();
    headers.extend(workbuddy_origin_headers(base));
    headers.extend(workbuddy_stainless_headers());
    headers.extend(workbuddy_conversation_headers(region));
    if uid.trim().is_empty() {
        headers.push(("X-No-User-Id", "1".into()));
    }
    if enterprise_id.trim().is_empty() {
        headers.push(("X-No-Enterprise-Id", "1".into()));
    }
    if domain.trim().is_empty() {
        headers.push(("X-No-Department-Info", "1".into()));
    }
    headers.retain(|(_, value)| !value.is_empty());
    headers
}

// ---------------------------------------------------------------------------
// Qoder
// ---------------------------------------------------------------------------

/// `cosy-clienttype` for inference and the OpenAPI family.
pub const QODER_CLIENT_TYPE: &str = "5";
/// The desktop client's link-local placeholder for `cosy-clientip`.
pub const QODER_CLIENT_IP: &str = "169.254.198.161";
/// `login-version` header the client sends alongside COSY.
pub const QODER_LOGIN_VERSION: &str = "v2";

/// 18-character `cosy-machinetype`, stable per uid.
pub fn qoder_machine_type(uid: &str) -> String {
    derive(uid, "machinetype")[..18].to_string()
}

/// `cosy-machinetoken`: base64url-shaped digest, stable per uid.
pub fn qoder_machine_token(uid: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use sha2::{Digest, Sha512};
    let uid = if uid.is_empty() { "anonymous" } else { uid };
    let digest = Sha512::digest(format!("machinetoken:{uid}").as_bytes());
    URL_SAFE_NO_PAD.encode(digest)[..43].to_string()
}

fn derive(uid: &str, salt: &str) -> String {
    use md5::{Digest, Md5};
    let uid = if uid.is_empty() { "anonymous" } else { uid };
    hex::encode(Md5::digest(format!("{salt}:{uid}").as_bytes()))
}

/// Identity headers for a COSY-signed inference or model-list request.
pub fn qoder_cosy_headers(uid: &str, cosy_version: &str) -> Vec<(&'static str, String)> {
    vec![
        ("cosy-data-policy", "AGREE".into()),
        ("cosy-machinetype", qoder_machine_type(uid)),
        ("cosy-clienttype", QODER_CLIENT_TYPE.into()),
        ("cosy-user", uid.to_string()),
        ("cosy-clientip", QODER_CLIENT_IP.into()),
        ("cosy-version", cosy_version.to_string()),
        ("cosy-machineid", derive(uid, "machine")),
        ("cosy-machinetoken", qoder_machine_token(uid)),
        ("login-version", QODER_LOGIN_VERSION.into()),
    ]
}

/// Best-effort client IP for a Qoder OpenAPI call. Absent means the header is
/// omitted rather than filled with a placeholder that is not even a real client.
pub fn qoder_client_ip(address: Option<IpAddr>) -> Option<String> {
    address.map(|address| address.to_string())
}

// ---------------------------------------------------------------------------
// TRAE
// ---------------------------------------------------------------------------

pub const TRAE_IDE_VERSION: &str = "0.1.52";
pub const TRAE_IDE_VERSION_CODE: &str = "20260811";
pub const TRAE_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
pub const TRAE_DEVICE_BRAND: &str = "83DG";
pub const TRAE_OS_VERSION: &str = "Windows 11 Pro";

pub fn trae_ua() -> String {
    format!("Trae/{TRAE_IDE_VERSION}")
}

/// The static IDE identity. These are client constants, not device identity:
/// `X-Machine-Id` / `X-Device-Id` stay whatever the operator imported.
pub fn trae_ide_headers() -> Vec<(&'static str, String)> {
    vec![
        ("X-App-Id", TRAE_APP_ID.into()),
        ("X-App-Version", "default".into()),
        ("X-Ide-Version", TRAE_IDE_VERSION.into()),
        ("X-Ide-Version-Code", TRAE_IDE_VERSION_CODE.into()),
        ("X-App-Version-Code", TRAE_IDE_VERSION_CODE.into()),
        ("X-Ide-Version-Type", "stable".into()),
        ("X-Device-Type", "windows".into()),
        ("X-OS-Version", TRAE_OS_VERSION.into()),
        ("X-Device-Brand", TRAE_DEVICE_BRAND.into()),
        ("Request-Traffic-Type", "prod".into()),
    ]
}

// ---------------------------------------------------------------------------

fn hex_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn hex_span() -> String {
    let simple = uuid::Uuid::new_v4().simple().to_string();
    simple[..16].to_string()
}

/// Apply `(name, value)` pairs, skipping any that reqwest rejects.
pub fn apply(headers: &mut reqwest::header::HeaderMap, pairs: Vec<(&'static str, String)>) {
    for (name, value) in pairs {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(&value),
        ) {
            headers.insert(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workbuddy_uas_follow_the_channel_of_each_realm() {
        assert!(workbuddy_chat_ua(Region::Cn).starts_with("WorkBuddy/5.9.3 WorkBuddy/5.9.3 CLI/"));
        assert!(workbuddy_chat_ua(Region::Intl).contains("workbuddy-ai/"));
        assert_eq!(workbuddy_desktop_ua(Region::Intl), "WorkBuddy/5.8.4");
    }

    #[test]
    fn workbuddy_conversation_ids_are_fresh_and_pairwise_consistent() {
        let first = workbuddy_conversation_headers(Region::Cn);
        let second = workbuddy_conversation_headers(Region::Cn);
        let get = |pairs: &[(&'static str, String)], name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_ne!(
            get(&first, "X-Conversation-ID"),
            get(&second, "X-Conversation-ID")
        );
        assert_eq!(
            get(&first, "X-Conversation-Request-ID"),
            get(&first, "X-Root-Request-ID")
        );
        assert_eq!(
            get(&first, "X-Conversation-Message-ID"),
            get(&first, "X-Request-ID")
        );
        assert_eq!(get(&first, "X-Conversation-Request-ID").len(), 32);
        // The trace parent embeds the same trace id it reports.
        assert!(get(&first, "traceparent").contains(&get(&first, "X-Trace-ID")));
    }

    #[test]
    fn qoder_device_identity_is_stable_per_uid_and_differs_between_accounts() {
        assert_eq!(qoder_machine_type("u1"), qoder_machine_type("u1"));
        assert_ne!(qoder_machine_type("u1"), qoder_machine_type("u2"));
        assert_eq!(qoder_machine_type("u1").len(), 18);
        assert_eq!(qoder_machine_token("u1").len(), 43);
        assert_eq!(qoder_machine_type(""), qoder_machine_type("anonymous"));
    }

    #[test]
    fn trae_headers_carry_the_client_constants() {
        let headers = trae_ide_headers();
        assert!(headers
            .iter()
            .any(|(name, value)| *name == "X-App-Id" && value == TRAE_APP_ID));
        assert_eq!(trae_ua(), "Trae/0.1.52");
    }

    #[test]
    fn apply_skips_unrepresentable_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        apply(
            &mut headers,
            vec![("x-ok", "1".into()), ("x-bad", "a\nb".into())],
        );
        assert_eq!(headers.get("x-ok").unwrap(), "1");
        assert!(headers.get("x-bad").is_none());
    }
}
