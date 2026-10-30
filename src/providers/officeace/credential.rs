//! OfficeAce credential model.
//!
//! Two credential families live in one JSON object because they serve different
//! planes with different lifetimes:
//!
//! | fields | lifetime | used for |
//! | --- | --- | --- |
//! | `model_app_key` / `model_app_secret` / `model_base_url` | permanent | chat and model catalogue (`Basic` auth) |
//! | `access_key_id` / `secret_access_key` / `security_token` / `project_id` | ~2 h | quota and check-in (`V11-HMAC-SHA256`) |
//! | `refresh_token` / `client_id` / `dpop_private_jwk` | rotated once | renewing the temporary AK/SK |
//!
//! Mixing the two is what produces `APIG.0303 app not found`: the STS token is
//! not a `Basic` credential. Secrets are `Zeroizing` and never print.

use std::fmt;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::accounts::Credential;
use crate::error::{ErrorKind, GatewayError};

use super::region;
use crate::providers::codearts::auth::DpopKey;

pub struct OfficeAceCredential {
    pub account_id: String,
    pub user_name: String,
    pub nickname: String,
    pub model_app_key: Zeroizing<String>,
    pub model_app_secret: Zeroizing<String>,
    pub model_base_url: String,
    pub access_key_id: Zeroizing<String>,
    pub secret_access_key: Zeroizing<String>,
    pub security_token: Zeroizing<String>,
    pub project_id: String,
    pub expiration: Option<DateTime<Utc>>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub client_id: String,
    pub dpop: Option<DpopKey>,
}

impl fmt::Debug for OfficeAceCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfficeAceCredential")
            .field("account_id", &self.account_id)
            .field("gateway", &self.model_base_url)
            .field("expiration", &self.expiration)
            .field("control_plane", &self.has_control_plane())
            .field("can_refresh", &self.can_refresh())
            .finish_non_exhaustive()
    }
}

impl OfficeAceCredential {
    pub fn from_credential(credential: &Credential) -> Result<Self, GatewayError> {
        Self::from_value(credential.to_json())
    }

    pub fn from_value(raw: &Value) -> Result<Self, GatewayError> {
        let model_app_key = text(raw, &["model_app_key", "modelAppKey"]);
        let model_app_secret = text(raw, &["model_app_secret", "modelAppSecret"]);
        if model_app_key.is_empty() || model_app_secret.is_empty() {
            return Err(GatewayError::new(
                ErrorKind::AuthInvalid,
                "OfficeAce credential has no chat app key; re-authorize the account",
            ));
        }
        Ok(Self {
            account_id: text(raw, &["account_id", "accountId"]),
            user_name: text(raw, &["user_name", "userName", "username"]),
            nickname: text(raw, &["nickname"]),
            model_app_key: Zeroizing::new(model_app_key),
            model_app_secret: Zeroizing::new(model_app_secret),
            model_base_url: text(raw, &["model_base_url", "modelBaseUrl"]),
            access_key_id: Zeroizing::new(text(raw, &["access_key_id", "accessKeyId"])),
            secret_access_key: Zeroizing::new(text(raw, &["secret_access_key", "secretAccessKey"])),
            security_token: Zeroizing::new(text(raw, &["security_token", "securityToken"])),
            project_id: text(raw, &["project_id", "projectId"]),
            expiration: expiration_of(raw),
            refresh_token: optional(raw, &["refresh_token", "refreshToken"]),
            client_id: {
                let value = text(raw, &["client_id", "clientId"]);
                if value.is_empty() {
                    region::CLIENT_ID.to_string()
                } else {
                    value
                }
            },
            dpop: raw.get("dpop_private_jwk").and_then(DpopKey::from_json),
        })
    }

    /// `Basic base64(app_key:app_secret)` for the model gateway.
    pub fn authorization(&self) -> String {
        let raw = format!(
            "{}:{}",
            self.model_app_key.as_str(),
            self.model_app_secret.as_str()
        );
        format!("Basic {}", STANDARD.encode(raw.as_bytes()))
    }

    /// Quota and check-in need the temporary AK/SK; chat does not.
    pub fn has_control_plane(&self) -> bool {
        !self.access_key_id.is_empty() && !self.secret_access_key.is_empty()
    }

    pub fn can_refresh(&self) -> bool {
        self.refresh_token.is_some() && self.dpop.is_some()
    }

    /// Unknown expiry is never treated as expired; the upstream answers with
    /// `APIG.0602` instead of pretending a token died early.
    pub fn expiring(&self, now: DateTime<Utc>, skew: chrono::Duration) -> bool {
        self.expiration
            .map(|expiration| expiration <= now + skew)
            .unwrap_or(false)
    }

    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        self.expiration
            .map(|expiration| expiration <= now)
            .unwrap_or(false)
    }

    /// Chat gateway for this account (credential host preferred).
    pub fn gateway(&self) -> String {
        if self.model_base_url.trim().is_empty() {
            region::DEFAULT_GATEWAY_HOST.to_string()
        } else {
            region::normalize_gateway(&self.model_base_url)
        }
    }
}

fn text(raw: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = raw.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn optional(raw: &Value, keys: &[&str]) -> Option<Zeroizing<String>> {
    let value = text(raw, keys);
    if value.is_empty() {
        None
    } else {
        Some(Zeroizing::new(value))
    }
}

fn expiration_of(raw: &Value) -> Option<DateTime<Utc>> {
    let value = raw.get("expiration")?;
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|value| value.with_timezone(&Utc));
    }
    value
        .as_i64()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn credential(raw: Value) -> OfficeAceCredential {
        OfficeAceCredential::from_value(&raw).expect("credential")
    }

    #[test]
    fn camel_case_and_snake_case_both_parse() {
        let parsed = credential(json!({
            "modelAppKey": "ak", "modelAppSecret": "sk", "modelBaseUrl": "https://gw/v2",
        }));
        assert_eq!(parsed.authorization(), "Basic YWs6c2s=");
        assert_eq!(parsed.gateway(), "https://gw");
        assert!(!parsed.has_control_plane());
    }

    #[test]
    fn a_credential_without_a_chat_key_is_rejected() {
        let error = OfficeAceCredential::from_value(&json!({"access_key_id": "ak"}))
            .expect_err("missing chat key");
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[test]
    fn control_plane_and_refresh_require_their_own_fields() {
        let parsed = credential(json!({
            "model_app_key": "ak", "model_app_secret": "sk",
            "access_key_id": "sts-ak", "secret_access_key": "sts-sk",
            "expiration": "2030-01-01T00:00:00Z",
        }));
        assert!(parsed.has_control_plane());
        assert!(!parsed.can_refresh());
        assert!(!parsed.expired(Utc::now()));
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let parsed = credential(json!({
            "model_app_key": "app-key", "model_app_secret": "app-secret",
            "access_key_id": "sts-ak", "secret_access_key": "sts-secret",
            "security_token": "sts-token", "refresh_token": "refresh-value",
        }));
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("app-secret"));
        assert!(!debug.contains("sts-secret"));
        assert!(!debug.contains("sts-token"));
        assert!(!debug.contains("refresh-value"));
    }
}
