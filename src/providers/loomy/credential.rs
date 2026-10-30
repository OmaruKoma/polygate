//! Loomy credential normalization.
//!
//! The model gateway is authenticated by the *login session* itself
//! (`useSessionAuth: true`), so a credential is one 32-character session string
//! plus the user id it belongs to. Both the flat PolyGate form
//! (`session`, `userid`, `expires_at`) and the desktop `auth-session.json`
//! spellings (`session`/`token`/`accessToken`, `userid`/`uid`) are accepted.
//!
//! There is no refresh token: when the session expires the operator imports a
//! new one. `expires_at` is advisory and only used to warn before expiry.

use std::fmt;

use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::error::GatewayError;

#[derive(Clone, PartialEq, Eq)]
pub struct LoomyCredential {
    pub session: String,
    pub userid: String,
    /// Unix seconds; `0` means unknown.
    pub expires_at: i64,
}

impl fmt::Debug for LoomyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoomyCredential")
            .field("session", &"<redacted>")
            .field("userid", &self.userid)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Drop for LoomyCredential {
    fn drop(&mut self) {
        self.session.zeroize();
    }
}

impl LoomyCredential {
    pub fn from_credential(credential: &Credential) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let session = first_string(value, &["session", "access_token", "accessToken", "token"])
            .ok_or_else(|| GatewayError::auth_invalid("Loomy credential has no session"))?;
        let session = session.trim().to_string();
        if session.is_empty() {
            return Err(GatewayError::auth_invalid("Loomy session is empty"));
        }
        let userid = first_string(value, &["userid", "uid", "user_id"]).unwrap_or_default();
        let expires_at = value
            .get("expires_at")
            .and_then(serde_json::Value::as_i64)
            .or_else(|| {
                value
                    .get("expireAt")
                    .and_then(serde_json::Value::as_i64)
                    .map(millis_to_seconds)
            })
            .unwrap_or(0);
        Ok(Self {
            session,
            userid,
            expires_at,
        })
    }

    /// Non-fatal freshness note for the admin UI.
    pub fn expiry_note(&self, now: i64) -> Option<String> {
        if self.expires_at <= 0 {
            return None;
        }
        let left = self.expires_at - now;
        if left <= 0 {
            Some("Loomy session is past its recorded expiry".into())
        } else if left < 86_400 {
            Some(format!("Loomy session expires in {}h", left / 3600))
        } else {
            None
        }
    }
}

fn millis_to_seconds(value: i64) -> i64 {
    if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(serde_json::Value::as_str) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn credential(value: serde_json::Value) -> Credential {
        Credential::from_value(value)
    }

    #[test]
    fn a_flat_session_and_userid_are_accepted() {
        let parsed = LoomyCredential::from_credential(&credential(json!({
            "session": "0123456789abcdef0123456789abcdef",
            "userid": "u-1",
            "expires_at": 1_800_000_000
        })))
        .expect("parse");
        assert_eq!(parsed.userid, "u-1");
        assert_eq!(parsed.expires_at, 1_800_000_000);
    }

    #[test]
    fn desktop_spellings_and_millisecond_expiry_are_accepted() {
        let parsed = LoomyCredential::from_credential(&credential(json!({
            "accessToken": "s",
            "uid": "u-2",
            "expireAt": 1_800_000_000_000i64
        })))
        .expect("parse");
        assert_eq!(parsed.session, "s");
        assert_eq!(parsed.userid, "u-2");
        assert_eq!(parsed.expires_at, 1_800_000_000);
    }

    #[test]
    fn a_credential_without_a_session_is_rejected() {
        let error = LoomyCredential::from_credential(&credential(json!({"uid": "u"})))
            .expect_err("no session");
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn expiry_notes_only_appear_close_to_expiry() {
        let mut cred =
            LoomyCredential::from_credential(&credential(json!({"session": "s"}))).expect("parse");
        assert!(cred.expiry_note(1_000).is_none());
        cred.expires_at = 1_000 + 3_600;
        assert!(cred.expiry_note(1_000).is_some_and(|n| n.contains("1h")));
        cred.expires_at = 10;
        assert!(cred.expiry_note(1_000).is_some_and(|n| n.contains("past")));
    }
}
