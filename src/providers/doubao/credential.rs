//! Doubao credential parsing.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md` §2. The upstream is authenticated by the
//! **login session cookies** plus a CSRF header taken from the same cookie jar:
//!
//! * `sessionid` / `sid_guard` / `ttwid` / `odin_tt` — the session itself;
//! * `passport_csrf_token` — echoed back as `x-tt-passport-csrf-token`;
//! * `device_id`, `web_id`, `fp` — device identity.
//!
//! The device identity is **not** generated here. Measured on 2026-11-13: a
//! request carrying only the session cookies is answered with
//! `710022004 rate limited` plus a slide-verification decision from
//! `shark_admin`; the same request with the desktop's own `device_id`/`web_id`/
//! `fp` succeeds. Inventing them would therefore only produce a permanently
//! "rate limited" account, so the operator imports the values that already
//! belong to their login.

use std::fmt;

use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::error::GatewayError;

/// Cookie names kept in the normalised jar. Anything else is dropped so a
/// pasted browser dump cannot leak unrelated cookies upstream.
const KEPT_COOKIES: [&str; 8] = [
    "sessionid",
    "sessionid_ss",
    "sid_guard",
    "sid_tt",
    "ttwid",
    "odin_tt",
    "passport_csrf_token",
    "passport_csrf_token_default",
];

#[derive(Clone, PartialEq, Eq)]
pub struct DoubaoCredential {
    cookies: Vec<(String, String)>,
    pub csrf_token: String,
    pub device_id: String,
    pub web_id: String,
    pub fp: String,
    pub ms_token: String,
}

impl fmt::Debug for DoubaoCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DoubaoCredential")
            .field("cookies", &format!("<{} items>", self.cookies.len()))
            .field("csrf_token", &"<redacted>")
            .field("device_id", &self.device_id)
            .field("web_id", &self.web_id)
            .field("fp", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Drop for DoubaoCredential {
    fn drop(&mut self) {
        for (_, value) in &mut self.cookies {
            value.zeroize();
        }
        self.csrf_token.zeroize();
        self.fp.zeroize();
        self.ms_token.zeroize();
    }
}

impl DoubaoCredential {
    pub fn from_credential(credential: &Credential) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let raw_cookies = value
            .get("cookies")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("cookie").and_then(serde_json::Value::as_str))
            .ok_or_else(|| GatewayError::auth_invalid("Doubao credential has no cookie string"))?;

        let pair = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };

        // Structured overrides win over values recovered from the cookie jar:
        // the desktop still carries the identity when the cookie string was
        // copied from a browser that does not have `sid_guard`.
        let device_id = pair("device_id")
            .or_else(|| pair("deviceId"))
            .unwrap_or_default();
        let web_id = pair("web_id").or_else(|| pair("webId")).unwrap_or_default();
        let fp = pair("fp").or_else(|| pair("user_fp")).unwrap_or_default();
        let ms_token = pair("ms_token")
            .or_else(|| pair("msToken"))
            .unwrap_or_default();

        let mut cookies = parse_cookie_string(raw_cookies);
        if cookies.iter().all(|(name, _)| name != "sessionid")
            && cookies.iter().all(|(name, _)| name != "sessionid_ss")
        {
            return Err(GatewayError::auth_invalid(
                "Doubao cookie string has no `sessionid`",
            ));
        }
        cookies.retain(|(name, _)| KEPT_COOKIES.contains(&name.as_str()));
        for (name, value) in cookies.iter_mut() {
            if name == "passport_csrf_token_default" {
                *name = "passport_csrf_token".to_string();
            }
            let _ = value;
        }

        let csrf_token = cookies
            .iter()
            .find(|(name, _)| name == "passport_csrf_token")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();

        Ok(Self {
            cookies,
            csrf_token,
            device_id,
            web_id,
            fp,
            ms_token,
        })
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Short, non-secret label for the admin UI.
    pub fn session_hint(&self) -> String {
        self.cookies
            .iter()
            .find(|(name, _)| name == "sessionid")
            .map(|(_, value)| {
                let visible: String = value.chars().take(6).collect();
                format!("{visible}…")
            })
            .unwrap_or_else(|| "<none>".to_string())
    }

    /// The session value itself, used only to derive a stable account id.
    pub fn sessionid(&self) -> &str {
        self.cookies
            .iter()
            .find(|(name, _)| name == "sessionid")
            .or_else(|| self.cookies.iter().find(|(name, _)| name == "sessionid_ss"))
            .map(|(_, value)| value.as_str())
            .unwrap_or_default()
    }

    /// The upstream requires the device identity; there is no safe fallback.
    pub fn missing_identity(&self) -> Option<&'static str> {
        if self.device_id.is_empty() {
            return Some("device_id");
        }
        if self.web_id.is_empty() {
            return Some("web_id");
        }
        if self.fp.is_empty() {
            return Some("fp");
        }
        None
    }
}

/// Accept both `a=b; c=d` and a dump that contains `Cookie:` in front.
fn parse_cookie_string(raw: &str) -> Vec<(String, String)> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("Cookie:").map(str::trim).unwrap_or(raw);
    raw.split(';')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn credential(value: serde_json::Value) -> Credential {
        Credential::from_value(value)
    }

    #[test]
    fn a_desktop_dump_is_normalised_into_the_kept_jar() {
        let parsed = DoubaoCredential::from_credential(&credential(json!({
            "cookies": "Cookie: sessionid=abc123; ttwid=t-1; passport_csrf_token=c-1; \
                        __ac_nonce=drop-me; odin_tt=o-1",
            "device_id": "d-1",
            "web_id": "w-1",
            "fp": "f-1"
        })))
        .expect("parse");
        assert_eq!(parsed.csrf_token, "c-1");
        assert_eq!(parsed.device_id, "d-1");
        assert_eq!(parsed.web_id, "w-1");
        assert!(!parsed.cookie_header().contains("__ac_nonce"));
        assert!(parsed.cookie_header().contains("sessionid=abc123"));
        assert_eq!(parsed.missing_identity(), None);
    }

    #[test]
    fn a_default_csrf_cookie_is_renamed() {
        let parsed = DoubaoCredential::from_credential(&credential(json!({
            "cookies": "sessionid=s; passport_csrf_token_default=c-2",
            "device_id": "d", "web_id": "w", "fp": "f"
        })))
        .expect("parse");
        assert_eq!(parsed.csrf_token, "c-2");
        assert!(parsed.cookie_header().contains("passport_csrf_token=c-2"));
        assert!(!parsed
            .cookie_header()
            .contains("passport_csrf_token_default"));
    }

    #[test]
    fn a_session_without_device_identity_is_reported_not_invented() {
        let parsed = DoubaoCredential::from_credential(&credential(json!({
            "cookies": "sessionid=s; passport_csrf_token=c"
        })))
        .expect("parse");
        assert_eq!(parsed.missing_identity(), Some("device_id"));
    }

    #[test]
    fn a_cookie_string_without_a_session_is_rejected() {
        let error = DoubaoCredential::from_credential(&credential(json!({
            "cookies": "ttwid=t"
        })))
        .expect_err("no sessionid");
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn debug_never_prints_the_secrets() {
        let parsed = DoubaoCredential::from_credential(&credential(json!({
            "cookies": "sessionid=supersecret; passport_csrf_token=csrfsecret",
            "device_id": "d", "web_id": "w", "fp": "fpsecret"
        })))
        .expect("parse");
        let text = format!("{parsed:?}");
        assert!(!text.contains("supersecret"));
        assert!(!text.contains("csrfsecret"));
        assert!(!text.contains("fpsecret"));
    }
}
