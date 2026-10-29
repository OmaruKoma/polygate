//! Qoder credential normalization.
//!
//! Source: Coding2API `src/provider/qoder/credential.py` at commit
//! `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! Tokens are `dt-`/`drt-` opaque strings. Expiry comes from `expires_in`
//! (milliseconds), then `expires_at` (RFC3339), then the access token's JWT
//! `exp` claim. If none is available the expiry is `0` (unknown); this code does
//! not invent a 30-day lifetime.
//!
//! Design decision for PolyGate: a login without a stable `uid` is rejected.
//! The reference implementation falls back to the first 16 characters of the
//! access token; doing that would make re-login look like a new device and can
//! correlate unrelated accounts.

use std::fmt;

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::DateTime;
use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::GatewayError;

use super::region;

pub const DEFAULT_USER_TYPE: &str = "personal_professional_trial";

/// A normalized Qoder device credential. Never serialized by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct QoderCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub uid: String,
    pub nickname: String,
    pub realm: Region,
    /// Unix seconds; `0` means unknown.
    pub expires_at: i64,
    pub auth_source: String,
    pub user_type: String,
    pub organization_id: String,
    pub organization_name: String,
    pub domain: String,
}

impl fmt::Debug for QoderCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QoderCredential")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("uid", &self.uid)
            .field("nickname", &self.nickname)
            .field("realm", &self.realm)
            .field("expires_at", &self.expires_at)
            .field("auth_source", &self.auth_source)
            .field("user_type", &self.user_type)
            .field("organization_id", &self.organization_id)
            .field("organization_name", &self.organization_name)
            .field("domain", &self.domain)
            .finish()
    }
}

impl Drop for QoderCredential {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl QoderCredential {
    /// Build a credential from a device-token response plus best-effort userinfo.
    pub fn from_device_at(
        realm: Region,
        device: &Value,
        userinfo: &Value,
        now: i64,
    ) -> Result<Self, GatewayError> {
        let access_token = device_token(device);
        if access_token.is_empty() {
            return Err(GatewayError::auth_invalid(
                "Qoder device authorization did not return an access token",
            ));
        }
        let uid = nonempty(userinfo, &["id", "userId", "uid"])
            .or_else(|| nonempty(device, &["user_id", "userId", "uid"]))
            .ok_or_else(|| {
                GatewayError::auth_invalid(
                    "Qoder authorization did not identify a user; start the login again",
                )
            })?;
        let configured = region::realm(realm);
        Ok(Self {
            access_token: access_token.clone(),
            refresh_token: nonempty(device, &["refresh_token", "refreshToken"]).unwrap_or_default(),
            uid,
            nickname: nonempty(userinfo, &["name", "nickname"]).unwrap_or_default(),
            realm,
            expires_at: expires_from_device(device, &access_token, now),
            auth_source: "oauth".into(),
            user_type: nonempty(userinfo, &["user_type", "userType"])
                .unwrap_or_else(|| DEFAULT_USER_TYPE.to_string()),
            organization_id: nonempty(userinfo, &["organization_id", "organizationId"])
                .unwrap_or_default(),
            organization_name: nonempty(userinfo, &["organization_name", "organizationName"])
                .unwrap_or_default(),
            domain: configured.domain.to_string(),
        })
    }

    /// Merge a refresh response into this credential, preserving identity fields
    /// (the upstream refresh response only carries the token family).
    pub fn merge_refreshed(&self, data: &Value, now: i64) -> Self {
        let access_token = {
            let candidate = device_token(data);
            if candidate.is_empty() {
                self.access_token.clone()
            } else {
                candidate
            }
        };
        let expires = expires_from_device(data, &access_token, now);
        let refresh_token = nonempty(data, &["refresh_token", "refreshToken"])
            .unwrap_or_else(|| self.refresh_token.clone());
        Self {
            access_token,
            refresh_token,
            uid: self.uid.clone(),
            nickname: self.nickname.clone(),
            realm: self.realm,
            expires_at: if expires > 0 {
                expires
            } else {
                self.expires_at
            },
            auth_source: self.auth_source.clone(),
            user_type: self.user_type.clone(),
            organization_id: self.organization_id.clone(),
            organization_name: self.organization_name.clone(),
            domain: self.domain.clone(),
        }
    }

    /// Access-token expiry, falling back to the JWT `exp` claim.
    pub fn token_expires_at(&self) -> i64 {
        if self.expires_at > 0 {
            self.expires_at
        } else {
            jwt_exp(&self.access_token).unwrap_or(0)
        }
    }

    /// Only OAuth credentials with a refresh token participate in refresh.
    pub fn needs_refresh(&self, now: i64, skew_seconds: i64) -> bool {
        self.auth_source == "oauth" && !self.refresh_token.is_empty() && {
            let expires = self.token_expires_at();
            expires > 0 && now.saturating_add(skew_seconds) >= expires
        }
    }

    /// Secret JSON written through `CredentialService`.
    pub fn to_json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "uid": self.uid,
            "nickname": self.nickname,
            "realm": self.realm.as_str(),
            "expires_at": self.expires_at,
            "auth_source": self.auth_source,
            "user_type": self.user_type,
            "organization_id": self.organization_id,
            "organization_name": self.organization_name,
            "domain": self.domain,
        })
    }

    /// Rebuild from the stored (already decrypted) credential payload.
    ///
    /// The access token and uid are mandatory; every other field falls back to
    /// a neutral default so an older row still loads. The realm is taken from
    /// the account row, never from the stored JSON, so a copied credential can
    /// not move an account between realms.
    pub fn from_credential(
        credential: &crate::accounts::Credential,
        realm: Region,
    ) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let access_token = nonempty(value, &["access_token", "token"])
            .ok_or_else(|| GatewayError::auth_invalid("Qoder credential has no access token"))?;
        let uid = nonempty(value, &["uid", "user_id"])
            .ok_or_else(|| GatewayError::auth_invalid("Qoder credential has no stable user id"))?;
        Ok(Self {
            access_token,
            refresh_token: nonempty(value, &["refresh_token"]).unwrap_or_default(),
            uid,
            nickname: nonempty(value, &["nickname", "name"]).unwrap_or_default(),
            realm,
            expires_at: value
                .get("expires_at")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0),
            auth_source: nonempty(value, &["auth_source"]).unwrap_or_else(|| "oauth".into()),
            user_type: nonempty(value, &["user_type"]).unwrap_or_else(|| DEFAULT_USER_TYPE.into()),
            organization_id: nonempty(value, &["organization_id"]).unwrap_or_default(),
            organization_name: nonempty(value, &["organization_name"]).unwrap_or_default(),
            domain: nonempty(value, &["domain"]).unwrap_or_default(),
        })
    }

    /// Display fallback when the operator did not provide an account name.
    pub fn name_hint(&self) -> String {
        if !self.nickname.trim().is_empty() {
            return self.nickname.trim().chars().take(64).collect();
        }
        let uid: String = self.uid.chars().take(8).collect();
        if uid.is_empty() {
            "Qoder account".into()
        } else {
            format!("Qoder {uid}")
        }
    }
}

fn device_token(data: &Value) -> String {
    nonempty(data, &["token", "device_token", "access_token"]).unwrap_or_default()
}

fn nonempty(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        data.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn expires_from_device(data: &Value, access_token: &str, now: i64) -> i64 {
    if let Some(value) = data.get("expires_in") {
        if !value.is_boolean() {
            let millis = value
                .as_i64()
                .or_else(|| value.as_f64().map(|number| number as i64))
                .unwrap_or(0);
            if millis > 0 {
                return now.saturating_add(millis / 1000);
            }
        }
    }
    if let Some(text) = data.get("expires_at").and_then(Value::as_str) {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(text.trim()) {
            return parsed.timestamp();
        }
    }
    jwt_exp(access_token).unwrap_or(0)
}

/// Read the `exp` claim without verifying the JWT. This is an expiry hint only;
/// the upstream is still the authority on whether a token is accepted.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let exp = value.get("exp")?;
    exp.as_i64()
        .or_else(|| exp.as_f64().map(|number| number as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Value {
        json!({
            "token": "dt-access",
            "refresh_token": "drt-refresh",
            "expires_in": 3_600_000,
            "user_id": "uid-1"
        })
    }

    #[test]
    fn expires_in_is_milliseconds() {
        let credential =
            QoderCredential::from_device_at(Region::Cn, &device(), &json!({}), 1_000).unwrap();
        assert_eq!(credential.expires_at, 1_000 + 3_600);
        assert_eq!(credential.uid, "uid-1");
        assert_eq!(credential.domain, "qoder.com.cn");
    }

    #[test]
    fn expires_at_and_jwt_are_fallbacks() {
        let mut data = device();
        data["expires_in"] = json!(null);
        data["expires_at"] = json!("2030-01-01T00:00:00Z");
        let credential =
            QoderCredential::from_device_at(Region::Cn, &data, &json!({}), 1_000).unwrap();
        assert_eq!(credential.expires_at, 1_893_456_000);

        let token = "x.eyJleHAiOjE4OTM0NTYwMDB9.y";
        let mut data = json!({"token": token, "user_id": "u"});
        data["expires_in"] = json!(null);
        let credential =
            QoderCredential::from_device_at(Region::Cn, &data, &json!({}), 1_000).unwrap();
        assert_eq!(credential.expires_at, 1_893_456_000);
    }

    #[test]
    fn missing_uid_is_rejected() {
        let data = json!({"token": "dt-access"});
        let error =
            QoderCredential::from_device_at(Region::Cn, &data, &json!({}), 1_000).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn refresh_merges_identity_and_tokens() {
        let credential = QoderCredential::from_device_at(
            Region::Cn,
            &device(),
            &json!({"id":"uid-1","name":"A"}),
            1_000,
        )
        .unwrap();
        let refreshed =
            credential.merge_refreshed(&json!({"token":"dt-new","expires_in":7_200_000}), 2_000);
        assert_eq!(refreshed.access_token, "dt-new");
        assert_eq!(refreshed.refresh_token, "drt-refresh");
        assert_eq!(refreshed.uid, "uid-1");
        assert_eq!(refreshed.nickname, "A");
        assert_eq!(refreshed.expires_at, 2_000 + 7_200);
        assert!(refreshed.needs_refresh(9_200, 30));
    }

    #[test]
    fn debug_never_prints_tokens() {
        let credential =
            QoderCredential::from_device_at(Region::Cn, &device(), &json!({}), 1_000).unwrap();
        let debug = format!("{credential:?}");
        assert!(!debug.contains("dt-access"));
        assert!(!debug.contains("drt-refresh"));
        assert!(debug.contains("uid-1"));
    }

    #[test]
    fn secret_json_contains_expected_fields() {
        let credential =
            QoderCredential::from_device_at(Region::Cn, &device(), &json!({}), 1_000).unwrap();
        let value = credential.to_json();
        assert_eq!(value["realm"], "cn");
        assert_eq!(value["access_token"], "dt-access");
        assert_eq!(value["refresh_token"], "drt-refresh");
        assert_eq!(value["uid"], "uid-1");
    }
}
