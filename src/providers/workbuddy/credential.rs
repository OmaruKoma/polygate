//! WorkBuddy credential normalization.
//!
//! Accepts both shapes an operator can supply:
//! * PolyGate's flat form (`access_token`, `uid`, `enterprise_id`, `domain`,
//!   `expires_at` in seconds);
//! * the desktop `.info` document (`auth.accessToken`, `account.uid`,
//!   `account.enterpriseId`, `auth.domain`, `auth.expiresAt` in milliseconds).
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §3.1. The desktop `$wbEncrypted`
//! envelope is **not** decrypted here; only plaintext credentials are accepted.
//!
//! The stored domain must belong to the account's region. A copied credential
//! can not move an account between realms, the same rule the Qoder adapter
//! applies to its realm.

use std::fmt;

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::Utc;
use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::config::Region;
use crate::error::GatewayError;

use super::region;

#[derive(Clone, PartialEq, Eq)]
pub struct WorkbuddyCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub uid: String,
    pub enterprise_id: String,
    pub domain: String,
    pub region: Region,
    /// Unix seconds; `0` means unknown. The JWT `exp` claim is the fallback.
    pub expires_at: i64,
    /// Unix seconds for the refresh token; `0` means unknown.
    pub refresh_expires_at: i64,
}

impl fmt::Debug for WorkbuddyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkbuddyCredential")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("uid", &self.uid)
            .field("enterprise_id", &self.enterprise_id)
            .field("domain", &self.domain)
            .field("region", &self.region)
            .field("expires_at", &self.expires_at)
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish()
    }
}

impl Drop for WorkbuddyCredential {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl WorkbuddyCredential {
    pub fn from_credential(credential: &Credential, region: Region) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let auth = value.get("auth");
        let account = value.get("account");

        let access_token = first_string(value, "access_token")
            .or_else(|| nested_string(auth, "accessToken"))
            .ok_or_else(|| {
                GatewayError::auth_invalid("WorkBuddy credential has no access token")
            })?;
        let uid = first_string(value, "uid")
            .or_else(|| nested_string(account, "uid"))
            .ok_or_else(|| GatewayError::auth_invalid("WorkBuddy credential has no user id"))?;

        let realm = region::realm(region);
        let stored_domain = first_string(value, "domain")
            .or_else(|| nested_string(auth, "domain"))
            .unwrap_or_default();
        if !realm.accepts_domain(&stored_domain) {
            return Err(GatewayError::auth_invalid(
                "WorkBuddy credential domain does not belong to the configured region",
            ));
        }
        let domain = if stored_domain.trim().is_empty() {
            realm.default_domain.to_string()
        } else {
            stored_domain.trim().trim_start_matches('.').to_string()
        };

        if let Some(subject) = jwt_claim(&access_token, "sub") {
            if !subject.is_empty() && subject != uid {
                return Err(GatewayError::auth_invalid(
                    "WorkBuddy credential user id does not match the token subject",
                ));
            }
        }

        Ok(Self {
            expires_at: expiry_from(value, auth, &access_token, Utc::now().timestamp()),
            access_token,
            refresh_token: first_string(value, "refresh_token")
                .or_else(|| nested_string(auth, "refreshToken"))
                .unwrap_or_default(),
            uid,
            enterprise_id: first_string(value, "enterprise_id")
                .or_else(|| nested_string(account, "enterpriseId"))
                .unwrap_or_default(),
            domain,
            region,
            refresh_expires_at: refresh_expiry_from(value, auth, Utc::now().timestamp()),
        })
    }

    pub fn token_expires_at(&self) -> i64 {
        if self.expires_at > 0 {
            self.expires_at
        } else {
            jwt_claim(&self.access_token, "exp")
                .and_then(|text| text.parse::<i64>().ok())
                .unwrap_or(0)
        }
    }

    pub fn is_expired(&self, now: i64) -> bool {
        let expires = self.token_expires_at();
        expires > 0 && now >= expires
    }

    /// True when the access token expires inside `skew_seconds`.
    pub fn needs_refresh(&self, now: i64, skew_seconds: i64) -> bool {
        let expires = self.token_expires_at();
        expires > 0 && now.saturating_add(skew_seconds) >= expires
    }

    pub fn can_refresh(&self, now: i64) -> bool {
        if self.refresh_token.is_empty() {
            return false;
        }
        self.refresh_expires_at == 0 || now < self.refresh_expires_at
    }

    /// Secret JSON written back through the credential store. Only normalized
    /// fields are kept; desktop PII and the `$wbEncrypted` envelope are dropped.
    pub fn to_json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "uid": self.uid,
            "enterprise_id": self.enterprise_id,
            "domain": self.domain,
            "region": self.region.as_str(),
            "expires_at": self.expires_at,
            "refresh_expires_at": self.refresh_expires_at,
            "auth_source": "workbuddy",
        })
    }

    /// Apply a refresh response. Identity fields stay; a rotated refresh token
    /// replaces the old one and an absent one keeps the current value.
    pub fn merge_refreshed(&self, data: &Value, now: i64) -> Result<Self, GatewayError> {
        let access_token = first_string(data, "accessToken")
            .or_else(|| first_string(data, "access_token"))
            .ok_or_else(|| {
                GatewayError::upstream_server("WorkBuddy refresh returned no access token")
            })?;
        let refresh_token = first_string(data, "refreshToken")
            .or_else(|| first_string(data, "refresh_token"))
            .unwrap_or_else(|| self.refresh_token.clone());
        let expires_at = match expiry_from(data, None, &access_token, now) {
            0 => self.expires_at,
            value => value,
        };
        let refresh_expires_at = match refresh_expiry_from(data, None, now) {
            0 => self.refresh_expires_at,
            value => value,
        };
        Ok(Self {
            access_token,
            refresh_token,
            uid: self.uid.clone(),
            enterprise_id: self.enterprise_id.clone(),
            domain: self.domain.clone(),
            region: self.region,
            expires_at,
            refresh_expires_at,
        })
    }

    pub fn name_hint(&self) -> String {
        let uid: String = self.uid.chars().take(8).collect();
        if uid.is_empty() {
            "WorkBuddy account".into()
        } else {
            format!("WorkBuddy {uid}")
        }
    }
}

fn refresh_expiry_from(value: &Value, auth: Option<&Value>, now: i64) -> i64 {
    if let Some(seconds) = value.get("refresh_expires_at").and_then(Value::as_i64) {
        if seconds > 0 {
            return normalize_epoch(seconds);
        }
    }
    if let Some(millis) = auth
        .and_then(|auth| auth.get("refreshExpiresAt"))
        .and_then(Value::as_i64)
    {
        if millis > 0 {
            return normalize_epoch(millis);
        }
    }
    let seconds = value
        .get("refresh_expires_in")
        .or_else(|| value.get("refreshExpiresIn"))
        .or_else(|| auth.and_then(|auth| auth.get("refreshExpiresIn")))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if seconds > 0 {
        now.saturating_add(seconds)
    } else {
        0
    }
}

fn first_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn nested_string(value: Option<&Value>, key: &str) -> Option<String> {
    value.and_then(|object| first_string(object, key))
}

fn expiry_from(value: &Value, auth: Option<&Value>, access_token: &str, now: i64) -> i64 {
    if let Some(seconds) = value.get("expires_at").and_then(Value::as_i64) {
        if seconds > 0 {
            return normalize_epoch(seconds);
        }
    }
    if let Some(millis) = auth
        .and_then(|auth| auth.get("expiresAt"))
        .and_then(Value::as_i64)
    {
        if millis > 0 {
            return normalize_epoch(millis);
        }
    }
    if let Some(seconds) = value
        .get("expires_in")
        .or_else(|| value.get("expiresIn"))
        .or_else(|| auth.and_then(|auth| auth.get("expiresIn")))
        .and_then(Value::as_i64)
    {
        if seconds > 0 {
            // The reference anchors a duration on `created_at` when present, so
            // a delayed response does not extend the real lifetime.
            let base = match value.get("created_at").and_then(Value::as_i64) {
                Some(created) if created > 0 => normalize_epoch(created),
                _ => now,
            };
            return base.saturating_add(seconds);
        }
    }
    jwt_claim(access_token, "exp")
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0)
}

/// The desktop file stores milliseconds; treating `1700000000000` as seconds
/// would push expiry ~50 000 years into the future.
fn normalize_epoch(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

/// Read one claim without verifying the signature. Used for expiry and for the
/// uid/subject cross-check only; the upstream remains the authority.
fn jwt_claim(token: &str, claim: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    match value.get(claim)? {
        Value::String(text) => Some(text.clone()),
        other if other.is_number() => Some(other.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn flat() -> Credential {
        Credential::from_value(json!({
            "access_token": "jwt-access",
            "uid": "uid-1",
            "enterprise_id": "ent-1",
            "domain": "www.codebuddy.cn",
            "expires_at": 1_900_000_000_i64
        }))
    }

    #[test]
    fn flat_credential_is_loaded() {
        let credential = WorkbuddyCredential::from_credential(&flat(), Region::Cn).unwrap();
        assert_eq!(credential.uid, "uid-1");
        assert_eq!(credential.enterprise_id, "ent-1");
        assert_eq!(credential.domain, "www.codebuddy.cn");
        assert_eq!(credential.expires_at, 1_900_000_000);
        assert!(!credential.is_expired(1_800_000_000));
        assert!(credential.is_expired(1_900_000_000));
    }

    #[test]
    fn desktop_info_shape_is_loaded_with_millisecond_expiry() {
        let credential = Credential::from_value(json!({
            "account": {"uid": "uid-2", "enterpriseId": "ent-2"},
            "auth": {
                "accessToken": "token",
                "domain": "www.codebuddy.cn",
                "expiresAt": 1_900_000_000_000_i64
            }
        }));
        let credential = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        assert_eq!(credential.uid, "uid-2");
        assert_eq!(credential.enterprise_id, "ent-2");
        assert_eq!(credential.expires_at, 1_900_000_000);
    }

    #[test]
    fn a_token_from_another_realm_is_rejected() {
        let credential = Credential::from_value(json!({
            "access_token": "t",
            "uid": "u",
            "domain": "www.workbuddy.ai"
        }));
        let error = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn uid_must_match_the_token_subject() {
        let token = "x.eyJzdWIiOiJ1aWQtb3RoZXIifQ.y";
        let credential = Credential::from_value(json!({
            "access_token": token,
            "uid": "uid-1",
            "domain": "www.codebuddy.cn"
        }));
        let error = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn jwt_exp_is_the_last_expiry_fallback() {
        let token = "x.eyJleHAiOjE4OTM0NTYwMDB9.y";
        let credential = Credential::from_value(json!({
            "access_token": token,
            "uid": "u",
            "domain": "www.codebuddy.cn"
        }));
        let credential = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        assert_eq!(credential.token_expires_at(), 1_893_456_000);
    }

    #[test]
    fn debug_never_prints_the_token() {
        let credential = Credential::from_value(json!({
            "access_token": "jwt-access",
            "refresh_token": "refresh-secret",
            "uid": "uid-1",
            "domain": "www.codebuddy.cn"
        }));
        let credential = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        let debug = format!("{credential:?}");
        assert!(!debug.contains("jwt-access"));
        assert!(!debug.contains("refresh-secret"));
        assert!(debug.contains("uid-1"));
    }

    #[test]
    fn refresh_material_and_rotation_are_tracked() {
        let credential = Credential::from_value(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "uid": "uid-1",
            "enterprise_id": "ent-1",
            "domain": "www.codebuddy.cn",
            "expires_at": 1_000,
            "refresh_expires_at": 9_000
        }));
        let credential = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        assert!(credential.needs_refresh(900, 200));
        assert!(!credential.needs_refresh(500, 200));
        assert!(credential.can_refresh(1_100));
        assert!(!credential.can_refresh(9_000));

        let refreshed = credential
            .merge_refreshed(
                &json!({"accessToken": "a2", "refreshToken": "r2", "expires_in": 3_600}),
                1_000,
            )
            .unwrap();
        assert_eq!(refreshed.access_token, "a2");
        assert_eq!(refreshed.refresh_token, "r2");
        assert_eq!(refreshed.uid, "uid-1");
        assert_eq!(refreshed.expires_at, 1_000 + 3_600);
        assert_eq!(refreshed.refresh_expires_at, 9_000);
    }

    #[test]
    fn a_refresh_response_without_a_refresh_token_keeps_the_old_one() {
        let credential = Credential::from_value(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "uid": "uid-1",
            "domain": "www.codebuddy.cn"
        }));
        let credential = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        let refreshed = credential
            .merge_refreshed(&json!({"accessToken": "a2"}), 1_000)
            .unwrap();
        assert_eq!(refreshed.refresh_token, "r1");
    }

    #[test]
    fn secret_json_round_trips_the_normalized_fields() {
        let credential = Credential::from_value(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "uid": "uid-1",
            "enterprise_id": "ent-1",
            "domain": "www.codebuddy.cn",
            "expires_at": 123
        }));
        let parsed = WorkbuddyCredential::from_credential(&credential, Region::Cn).unwrap();
        let reloaded = Credential::from_value(parsed.to_json());
        let again = WorkbuddyCredential::from_credential(&reloaded, Region::Cn).unwrap();
        assert_eq!(again, parsed);
        assert_eq!(again.to_json()["access_token"], "a1");
    }
}
