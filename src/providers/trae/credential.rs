//! TRAE credential normalization.
//!
//! Accepts the two shapes an operator can supply, exactly as the reference
//! parser does (`Coding2API/src/provider/trae/credential.py:75-108`):
//! * flat (`accessToken`, `refreshToken`, `uid`, `domain`, `apiHost`, …);
//! * nested (`auth.accessToken`, `account.uid`, …), the shape TRAE's own IDE
//!   writes to disk.
//!
//! Compatibility boundary: `machineId`/`deviceId` are carried **only** when the
//! operator imported them. This adapter never generates or substitutes device
//! identifiers — that would be device-fingerprint forgery, which the project
//! forbids (AGENTS.md §2).
//!
//! `apiHost` is operator-controlled, so it is checked against the realm before
//! it is ever stored or used. An out-of-realm host is rejected at import instead
//! of being silently replaced, so the operator sees the problem immediately.

use std::fmt;

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::config::Region;
use crate::error::GatewayError;

use super::region;

#[derive(Clone, PartialEq, Eq)]
pub struct TraeCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub uid: String,
    /// The realmail domain the token belongs to (`trae.cn` / `trae.ai`).
    pub domain: String,
    /// Operator-supplied OAuth host override; validated against the realm.
    pub api_host: String,
    /// Imported device identity. Never generated here; empty means absent.
    pub machine_id: String,
    pub device_id: String,
    pub enterprise_id: String,
    pub nickname: String,
    /// Unix seconds; `0` means unknown. The JWT `exp` claim is the fallback.
    pub expires_at: i64,
    pub region: Region,
}

impl fmt::Debug for TraeCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraeCredential")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("uid", &self.uid)
            .field("domain", &self.domain)
            .field("api_host", &self.api_host)
            .field("machine_id", &self.machine_id)
            .field("device_id", &self.device_id)
            .field("enterprise_id", &self.enterprise_id)
            .field("expires_at", &self.expires_at)
            .field("region", &self.region)
            .finish()
    }
}

impl Drop for TraeCredential {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl TraeCredential {
    pub fn from_credential(credential: &Credential, region: Region) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let auth = value.get("auth");
        let account = value.get("account");
        if value.get("auth").is_some()
            && (auth.is_some_and(|v| !v.is_object()) || account.is_some_and(|v| !v.is_object()))
        {
            return Err(GatewayError::auth_invalid(
                "TRAE credential `auth`/`account` sections must be objects",
            ));
        }

        let access_token = first_string(value, "access_token")
            .or_else(|| first_string(value, "accessToken"))
            .or_else(|| nested_string(auth, "accessToken"))
            .ok_or_else(|| GatewayError::auth_invalid("TRAE credential has no access token"))?;
        let uid = first_string(value, "uid")
            .or_else(|| nested_string(account, "uid"))
            .or_else(|| nested_string(account, "userId"))
            .ok_or_else(|| GatewayError::auth_invalid("TRAE credential has no user id"))?;

        let realm = region::realm(region);
        let stored_domain = first_string(value, "domain")
            .or_else(|| nested_string(auth, "domain"))
            .unwrap_or_default();
        if !realm.accepts_domain(&stored_domain) {
            return Err(GatewayError::auth_invalid(
                "TRAE credential domain does not belong to the configured region",
            ));
        }
        let domain = if stored_domain.trim().is_empty() {
            realm.default_domain.to_string()
        } else {
            stored_domain.trim().trim_start_matches('.').to_string()
        };

        let api_host = first_string(value, "api_host")
            .or_else(|| first_string(value, "apiHost"))
            .or_else(|| nested_string(auth, "apiHost"))
            .unwrap_or_default();
        if !realm.accepts_api_host(&api_host) {
            return Err(GatewayError::auth_invalid(
                "TRAE credential apiHost is not an official host of the configured region",
            ));
        }

        if let Some(subject) = jwt_claim(&access_token, "sub") {
            if !subject.is_empty() && subject != uid {
                return Err(GatewayError::auth_invalid(
                    "TRAE credential user id does not match the token subject",
                ));
            }
        }

        Ok(Self {
            expires_at: expiry_from(value, auth),
            access_token,
            refresh_token: first_string(value, "refresh_token")
                .or_else(|| first_string(value, "refreshToken"))
                .or_else(|| nested_string(auth, "refreshToken"))
                .unwrap_or_default(),
            uid,
            domain,
            api_host: api_host.trim().trim_end_matches('/').to_string(),
            machine_id: first_string(value, "machine_id")
                .or_else(|| first_string(value, "machineId"))
                .or_else(|| nested_string(auth, "machineId"))
                .unwrap_or_default(),
            device_id: first_string(value, "device_id")
                .or_else(|| first_string(value, "deviceId"))
                .or_else(|| nested_string(auth, "deviceId"))
                .unwrap_or_default(),
            enterprise_id: first_string(value, "enterprise_id")
                .or_else(|| first_string(value, "enterpriseId"))
                .or_else(|| nested_string(account, "enterpriseId"))
                .unwrap_or_default(),
            nickname: first_string(value, "nickname")
                .or_else(|| nested_string(account, "nickname"))
                .unwrap_or_default(),
            region,
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

    /// True when the access token expires inside `skew_seconds`. An unknown
    /// expiry is treated as needing a refresh, matching the reference: it is
    /// safer to refresh once than to send a token whose lifetime is unknown.
    pub fn needs_refresh(&self, now: i64, skew_seconds: i64) -> bool {
        let expires = self.token_expires_at();
        expires <= 0 || now.saturating_add(skew_seconds) >= expires
    }

    pub fn can_refresh(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// Keep identity and device fields; replace the token and expiry with a
    /// successful ExchangeToken answer.
    pub fn merge_refreshed(&self, data: &Value, now: i64) -> Result<Self, GatewayError> {
        let access_token = first_string(data, "Token")
            .or_else(|| first_string(data, "token"))
            .or_else(|| first_string(data, "accessToken"))
            .ok_or_else(|| {
                GatewayError::upstream_server("TRAE token exchange returned no access token")
            })?;
        let refresh_token = first_string(data, "RefreshToken")
            .or_else(|| first_string(data, "refreshToken"))
            .unwrap_or_else(|| self.refresh_token.clone());
        let expires_at = exchange_expiry(data, &access_token, now);
        Ok(Self {
            access_token,
            refresh_token,
            uid: self.uid.clone(),
            domain: self.domain.clone(),
            api_host: self.api_host.clone(),
            machine_id: self.machine_id.clone(),
            device_id: self.device_id.clone(),
            enterprise_id: self.enterprise_id.clone(),
            nickname: self.nickname.clone(),
            expires_at,
            region: self.region,
        })
    }

    pub fn name_hint(&self) -> String {
        let uid: String = self.uid.chars().take(8).collect();
        if uid.is_empty() {
            "TRAE account".into()
        } else {
            format!("TRAE {uid}")
        }
    }

    /// Secret JSON written back through the credential store. Only normalized
    /// fields are kept; no derived JWT expiry is stored, so a refreshed token
    /// cannot inherit a stale authoritative expiry.
    pub fn to_json(&self) -> Value {
        json!({
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "uid": self.uid,
            "domain": self.domain,
            "api_host": self.api_host,
            "machine_id": self.machine_id,
            "device_id": self.device_id,
            "enterprise_id": self.enterprise_id,
            "nickname": self.nickname,
            "expires_at": self.expires_at,
            "region": self.region.as_str(),
            "auth_source": "trae",
        })
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

fn expiry_from(value: &Value, auth: Option<&Value>) -> i64 {
    let explicit = value
        .get("expires_at")
        .or_else(|| value.get("expiresAt"))
        .and_then(Value::as_i64)
        .or_else(|| {
            auth.and_then(|auth| auth.get("expiresAt"))
                .and_then(Value::as_i64)
        });
    explicit
        .filter(|seconds| *seconds > 0)
        .map(normalize_epoch)
        .unwrap_or(0)
}

/// The IDE stores milliseconds; treating `1700000000000` as seconds would push
/// expiry ~50 000 years into the future.
fn normalize_epoch(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

/// `TokenExpireAt` is an epoch (the IDE sends milliseconds), while
/// `TokenExpireDuration` is seconds from now. A response carrying neither
/// falls back to the new token's JWT `exp`.
fn exchange_expiry(data: &Value, access_token: &str, now: i64) -> i64 {
    if let Some(at) = data
        .get("TokenExpireAt")
        .or_else(|| data.get("tokenExpireAt"))
        .and_then(Value::as_i64)
        .filter(|at| *at > 0)
    {
        return normalize_epoch(at);
    }
    if let Some(duration) = data
        .get("TokenExpireDuration")
        .or_else(|| data.get("tokenExpireDuration"))
        .and_then(Value::as_i64)
        .filter(|duration| *duration > 0)
    {
        return now.saturating_add(duration);
    }
    jwt_claim(access_token, "exp")
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0)
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

    fn flat() -> Credential {
        Credential::from_value(json!({
            "access_token": "jwt-access",
            "uid": "uid-1",
            "domain": "trae.cn",
            "expires_at": 1_900_000_000_i64
        }))
    }

    #[test]
    fn flat_credential_is_loaded() {
        let credential = TraeCredential::from_credential(&flat(), Region::Cn).unwrap();
        assert_eq!(credential.uid, "uid-1");
        assert_eq!(credential.domain, "trae.cn");
        assert_eq!(credential.api_host, "");
        assert_eq!(credential.expires_at, 1_900_000_000);
        assert!(!credential.is_expired(1_800_000_000));
        assert!(credential.is_expired(1_900_000_000));
    }

    #[test]
    fn nested_ide_shape_is_loaded_with_millisecond_expiry() {
        let credential = Credential::from_value(json!({
            "account": {"uid": "uid-2", "enterpriseId": "ent-2", "nickname": "nick"},
            "auth": {
                "accessToken": "token",
                "refreshToken": "refresh",
                "domain": "trae.cn",
                "apiHost": "https://api.trae.com.cn/",
                "machineId": "abc",
                "deviceId": "def",
                "expiresAt": 1_900_000_000_000_i64
            }
        }));
        let credential = TraeCredential::from_credential(&credential, Region::Cn).unwrap();
        assert_eq!(credential.uid, "uid-2");
        assert_eq!(credential.enterprise_id, "ent-2");
        assert_eq!(credential.nickname, "nick");
        assert_eq!(credential.refresh_token, "refresh");
        assert_eq!(credential.api_host, "https://api.trae.com.cn");
        assert_eq!(credential.machine_id, "abc");
        assert_eq!(credential.device_id, "def");
        assert_eq!(credential.expires_at, 1_900_000_000);
    }

    #[test]
    fn missing_device_identifiers_stay_empty() {
        let credential = TraeCredential::from_credential(&flat(), Region::Cn).unwrap();
        assert!(credential.machine_id.is_empty());
        assert!(credential.device_id.is_empty());
    }

    #[test]
    fn a_token_from_another_realm_is_rejected() {
        let credential = Credential::from_value(json!({
            "access_token": "t",
            "uid": "u",
            "domain": "trae.ai"
        }));
        let error = TraeCredential::from_credential(&credential, Region::Cn).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);

        let intl = Credential::from_value(json!({
            "access_token": "t",
            "uid": "u",
            "domain": "trae.cn"
        }));
        let error = TraeCredential::from_credential(&intl, Region::Intl).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn an_out_of_realm_api_host_is_rejected() {
        let credential = Credential::from_value(json!({
            "accessToken": "t",
            "uid": "u",
            "domain": "trae.cn",
            "apiHost": "https://attacker.example.com"
        }));
        let error = TraeCredential::from_credential(&credential, Region::Cn).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn uid_must_match_the_token_subject() {
        let token = "x.eyJzdWIiOiJ1aWQtb3RoZXIifQ.y";
        let credential = Credential::from_value(json!({
            "access_token": token,
            "uid": "uid-1",
            "domain": "trae.cn"
        }));
        let error = TraeCredential::from_credential(&credential, Region::Cn).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
    }

    #[test]
    fn jwt_exp_is_the_last_expiry_fallback() {
        let token = "x.eyJleHAiOjE4OTM0NTYwMDB9.y";
        let credential = Credential::from_value(json!({
            "access_token": token,
            "uid": "u",
            "domain": "trae.cn"
        }));
        let credential = TraeCredential::from_credential(&credential, Region::Cn).unwrap();
        assert_eq!(credential.expires_at, 0);
        assert_eq!(credential.token_expires_at(), 1_893_456_000);
    }

    #[test]
    fn debug_never_prints_the_token() {
        let credential = Credential::from_value(json!({
            "access_token": "jwt-access",
            "refresh_token": "refresh-secret",
            "uid": "uid-1",
            "domain": "trae.cn"
        }));
        let credential = TraeCredential::from_credential(&credential, Region::Cn).unwrap();
        let debug = format!("{credential:?}");
        assert!(!debug.contains("jwt-access"));
        assert!(!debug.contains("refresh-secret"));
        assert!(debug.contains("uid-1"));
    }

    #[test]
    fn secret_json_round_trips_the_normalized_fields() {
        let parsed = TraeCredential::from_credential(&flat(), Region::Cn).unwrap();
        let reloaded = Credential::from_value(parsed.to_json());
        let again = TraeCredential::from_credential(&reloaded, Region::Cn).unwrap();
        assert_eq!(again, parsed);
        assert_eq!(again.to_json()["access_token"], "jwt-access");
        assert_eq!(again.to_json()["auth_source"], "trae");
    }

    #[test]
    fn refresh_window_and_rotation_keep_identity() {
        let credential = Credential::from_value(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "uid": "uid-1",
            "domain": "trae.cn",
            "machineId": "machine-1",
            "deviceId": "device-1",
            "expires_at": 1_000
        }));
        let credential = TraeCredential::from_credential(&credential, Region::Cn).unwrap();
        assert!(credential.needs_refresh(900, 200));
        assert!(!credential.needs_refresh(500, 200));
        assert!(credential.can_refresh());

        let refreshed = credential
            .merge_refreshed(
                &json!({
                    "Token": "a2",
                    "RefreshToken": "r2",
                    "TokenExpireAt": 1_900_000_000_000_i64
                }),
                1_000,
            )
            .unwrap();
        assert_eq!(refreshed.access_token, "a2");
        assert_eq!(refreshed.refresh_token, "r2");
        assert_eq!(refreshed.expires_at, 1_900_000_000);
        assert_eq!(refreshed.uid, "uid-1");
        assert_eq!(refreshed.machine_id, "machine-1");
        assert_eq!(refreshed.device_id, "device-1");
        assert_eq!(refreshed.region, Region::Cn);
    }

    #[test]
    fn an_unknown_expiry_is_refreshed_conservatively() {
        let unknown = Credential::from_value(json!({
            "access_token": "opaque-token",
            "uid": "uid-1",
            "domain": "trae.cn"
        }));
        let credential = TraeCredential::from_credential(&unknown, Region::Cn).unwrap();
        assert_eq!(credential.token_expires_at(), 0);
        assert!(credential.needs_refresh(1_000, 300));
        assert!(!credential.is_expired(1_000));
        assert!(!credential.can_refresh());
    }

    #[test]
    fn exchange_duration_is_anchored_to_now() {
        let credential = TraeCredential::from_credential(&flat(), Region::Cn).unwrap();
        let refreshed = credential
            .merge_refreshed(&json!({"Token": "a2", "TokenExpireDuration": 3_600}), 1_000)
            .unwrap();
        assert_eq!(refreshed.expires_at, 4_600);
        assert_eq!(refreshed.refresh_token, "");
    }
}
