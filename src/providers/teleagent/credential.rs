//! TeleAgent credential normalization.
//!
//! The login token is `<tenantId>:<JWT>` (`X-Token`). Requests are signed with
//! the JWT's third segment, so the token is a secret: it is zeroized on drop
//! and never printed. The device identity (`deviceId` / `installId`) is sent
//! upstream but the server does not validate it (measured: any UUID passes);
//! it is kept because the client always sends it.
//!
//! Accepted spellings cover the flat PolyGate form and the desktop
//! `device-meta.json` / `token.json` fields the extractor writes.

use std::fmt;

use zeroize::Zeroize;

use crate::accounts::Credential;
use crate::error::GatewayError;

use super::region;

#[derive(Clone, PartialEq, Eq)]
pub struct TeleagentCredential {
    /// `<tenant>:<JWT>` exactly as the client stores it.
    pub token: String,
    pub device_id: String,
    pub install_id: String,
    pub app_version: String,
}

impl fmt::Debug for TeleagentCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeleagentCredential")
            .field("token", &"<redacted>")
            .field("device_id", &self.device_id)
            .field("install_id", &self.install_id)
            .field("app_version", &self.app_version)
            .finish()
    }
}

impl Drop for TeleagentCredential {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl TeleagentCredential {
    pub fn from_credential(credential: &Credential) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let token = first_string(
            value,
            &["token", "x_token", "xToken", "jwt", "encryptedToken"],
        )
        .ok_or_else(|| {
            GatewayError::auth_invalid(
                "TeleAgent credential has no login token; import `<tenant>:<JWT>`",
            )
        })?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(GatewayError::auth_invalid("TeleAgent token is empty"));
        }
        // The signature key is the JWT's third segment; a two-segment token can
        // never sign a request the upstream accepts.
        if !token.contains(':') || token.matches('.').count() < 2 {
            return Err(GatewayError::auth_invalid(
                "TeleAgent token must be `<tenant>:<JWT>` with three dot-separated segments",
            ));
        }
        let app_version = first_string(value, &["app_version", "appVersion", "version"])
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| region::DEFAULT_APP_VERSION.to_string());
        Ok(Self {
            token,
            device_id: first_string(value, &["device_id", "deviceId"]).unwrap_or_default(),
            install_id: first_string(value, &["install_id", "installId"]).unwrap_or_default(),
            app_version,
        })
    }

    /// JWT third segment; the whole string when the token is not three-part.
    pub fn signing_key(&self) -> &str {
        self.token.rsplit('.').next().unwrap_or(self.token.as_str())
    }

    /// Account hint for the admin list: the tenant plus the JWT signature tail.
    pub fn fingerprint(&self) -> String {
        let tenant = self.token.split_once(':').map(|(t, _)| t).unwrap_or("?");
        let signature = self.signing_key();
        let tail: String = {
            let reversed: String = signature.chars().rev().take(8).collect();
            reversed.chars().rev().collect()
        };
        format!("{tenant}:{tail}")
    }

    /// `exp` from the JWT payload, when it decodes. Advisory only: the upstream
    /// is the authority on whether the token is still accepted.
    pub fn token_expiry(&self) -> Option<i64> {
        let jwt = self
            .token
            .split_once(':')
            .map(|(_, jwt)| jwt)
            .unwrap_or(&self.token);
        let payload = jwt.split('.').nth(1)?;
        let bytes = base64url_decode(payload)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        value.get("exp").and_then(serde_json::Value::as_i64)
    }
}

/// Minimal base64url decode (JWT segments are unpadded).
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tenant_jwt_pair_is_accepted() {
        let credential = Credential::from_value(json!({
            "token": "Em9xR4:aaa.bbb.ccc",
            "device_id": "d1",
            "install_id": "i1",
        }));
        let parsed = TeleagentCredential::from_credential(&credential).expect("credential");
        assert_eq!(parsed.signing_key(), "ccc");
        assert_eq!(parsed.app_version, region::DEFAULT_APP_VERSION);
        assert_eq!(parsed.device_id, "d1");
        assert_eq!(parsed.fingerprint(), "Em9xR4:ccc");
    }

    #[test]
    fn a_bare_token_is_rejected() {
        let credential = Credential::from_value(json!({"token": "not-a-jwt"}));
        let error = TeleagentCredential::from_credential(&credential).expect_err("reject");
        assert!(error.message.contains("<tenant>:<JWT>"));
    }

    #[test]
    fn a_missing_token_is_rejected() {
        let credential = Credential::from_value(json!({"device_id": "d1"}));
        let error = TeleagentCredential::from_credential(&credential).expect_err("reject");
        assert!(error.message.contains("no login token"));
    }
}
