//! CodeArts credential model, expiry detection and STS refresh.
//!
//! Evidence (read-only reference sources):
//! * local `app/codearts/auth.py` — refresh form fields, `client_id`, DPoP;
//! * Coding2API `src/provider/codearts/auth.py` / `dpop.py` — ES256 DPoP proof,
//!   low-S normalization requirement, terminal refresh errors;
//! * Coding2API `src/provider/codearts/events.py` — `client_id` is bound to the
//!   refresh token, so the original DPoP key and PKCE verifier must be reused.
//!
//! A credential that has no refresh material can still be used until it
//! expires; after that the provider reports `AuthExpired` and the operator must
//! re-import it. No login, captcha or device-flow code lives here.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::accounts::Credential;
use crate::error::{ErrorKind, GatewayError};

/// Provider-side refresh tolerance: refresh slightly before the STS token
/// actually expires so an in-flight request is not rejected mid-stream.
pub const REFRESH_SKEW: Duration = Duration::seconds(60);

const CLIENT_ID: &str = "codearts-agent";
const TOKEN_PATH: &str = "/v1/oauth2/tokens";

/// Decoded view of one CodeArts credential JSON object.
pub struct CodeArtsCredential {
    pub access_key_id: String,
    pub secret_access_key: Zeroizing<String>,
    pub security_token: Zeroizing<String>,
    pub domain_id: String,
    pub expiration: Option<DateTime<Utc>>,
    pub refresh_token: Option<Zeroizing<String>>,
    pub code_verifier: Option<Zeroizing<String>>,
    pub dpop: Option<DpopKey>,
}

impl fmt::Debug for CodeArtsCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeArtsCredential")
            .field("access_key_id", &"<redacted>")
            .field("domain_id", &self.domain_id)
            .field("expiration", &self.expiration)
            .field("can_refresh", &self.can_refresh())
            .finish_non_exhaustive()
    }
}

pub struct DpopKey {
    scalar: Zeroizing<[u8; 32]>,
    pub x: String,
    pub y: String,
}

impl fmt::Debug for DpopKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DpopKey(<redacted>)")
    }
}

impl CodeArtsCredential {
    pub fn parse(credential: &Credential) -> Result<Self, GatewayError> {
        let value = credential.to_json();
        let access_key_id = credential.require("access_key_id")?.to_string();
        let secret_access_key =
            Zeroizing::new(credential.require("secret_access_key")?.to_string());
        let security_token = Zeroizing::new(
            credential
                .get("security_token")
                .unwrap_or_default()
                .to_string(),
        );
        let domain_id = credential.get("domain_id").unwrap_or_default().to_string();
        let expiration = credential
            .get("expiration")
            .and_then(parse_rfc3339)
            .or_else(|| credential.get("expires_at").and_then(parse_rfc3339));
        let refresh_token = credential
            .get("refresh_token")
            .filter(|value| !value.trim().is_empty())
            .map(|value| Zeroizing::new(value.to_string()));
        let code_verifier = credential
            .get("code_verifier")
            .filter(|value| !value.trim().is_empty())
            .map(|value| Zeroizing::new(value.to_string()));
        let dpop = value.get("dpop_private_jwk").and_then(DpopKey::from_json);
        Ok(Self {
            access_key_id,
            secret_access_key,
            security_token,
            domain_id,
            expiration,
            refresh_token,
            code_verifier,
            dpop,
        })
    }

    pub fn can_refresh(&self) -> bool {
        self.refresh_token.is_some() && self.dpop.is_some()
    }

    /// Whether the STS token is expired or close enough that a refresh should
    /// happen before the next upstream call. Unknown expiry is never treated as
    /// expired; the upstream will answer with `APIG.0602` instead.
    pub fn expiring(&self, now: DateTime<Utc>) -> bool {
        self.expiration
            .map(|expiration| expiration <= now + REFRESH_SKEW)
            .unwrap_or(false)
    }

    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        self.expiration
            .map(|expiration| expiration <= now)
            .unwrap_or(false)
    }

    /// Sign an STS refresh request and return the rotated credential JSON.
    pub async fn refresh(
        &self,
        http: &reqwest::Client,
        sts_url: &str,
        original: &Credential,
        now: DateTime<Utc>,
    ) -> Result<Credential, GatewayError> {
        let refresh_token = self.refresh_token.as_ref().ok_or_else(|| {
            GatewayError::new(
                ErrorKind::AuthExpired,
                "credential has no refresh token; re-login is required",
            )
        })?;
        let dpop = self.dpop.as_ref().ok_or_else(|| {
            GatewayError::new(
                ErrorKind::AuthExpired,
                "credential has no DPoP key; re-login is required",
            )
        })?;
        let token_url = format!("{}{TOKEN_PATH}", sts_url.trim_end_matches('/'));
        let proof = dpop.proof(&token_url, now)?;

        let mut form = vec![
            ("client_id", CLIENT_ID.to_string()),
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
        ];
        if let Some(verifier) = &self.code_verifier {
            form.push(("code_verifier", verifier.to_string()));
        }

        let response = http
            .post(&token_url)
            .header("DPoP", proof)
            .form(&form)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    GatewayError::timeout("CodeArts token refresh timed out")
                } else {
                    GatewayError::transport("CodeArts token refresh failed")
                }
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(classify_refresh_failure(status.as_u16(), &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("CodeArts token endpoint returned a non-JSON response")
        })?;
        let credentials = payload.get("credentials").ok_or_else(|| {
            GatewayError::upstream_server("CodeArts token response is missing credentials")
        })?;

        let required = |key: &str| -> Result<&str, GatewayError> {
            credentials
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    GatewayError::upstream_server(format!(
                        "CodeArts token response is missing {key}"
                    ))
                })
        };

        // Preserve fields this adapter does not understand (ticket state,
        // operator annotations) without keeping a second plaintext copy on the
        // parsed credential.
        let mut updated = original.to_json().clone();
        if let Some(object) = updated.as_object_mut() {
            object.insert("access_key_id".into(), json!(required("access_key_id")?));
            object.insert(
                "secret_access_key".into(),
                json!(required("secret_access_key")?),
            );
            object.insert("security_token".into(), json!(required("security_token")?));
            if let Some(expiration) = credentials.get("expiration").and_then(Value::as_str) {
                object.insert("expiration".into(), json!(expiration));
            }
            if let Some(rotated) = payload.get("refresh_token").and_then(Value::as_str) {
                if !rotated.is_empty() {
                    object.insert("refresh_token".into(), json!(rotated));
                }
            }
        }
        Ok(Credential::from_value(updated))
    }
}

impl DpopKey {
    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let d = object.get("d")?.as_str()?;
        if object.get("kty").and_then(Value::as_str) != Some("EC")
            || object.get("crv").and_then(Value::as_str) != Some("P-256")
        {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(d).ok()?;
        let scalar: [u8; 32] = bytes.try_into().ok()?;
        Some(Self {
            scalar: Zeroizing::new(scalar),
            x: object.get("x").and_then(Value::as_str)?.to_string(),
            y: object.get("y").and_then(Value::as_str)?.to_string(),
        })
    }

    /// RFC 9449 DPoP proof. RustCrypto's ECDSA signer emits low-S signatures,
    /// which the Huawei STS requires (see Coding2API `dpop.py`).
    pub(crate) fn proof(&self, url: &str, now: DateTime<Utc>) -> Result<String, GatewayError> {
        let header = json!({
            "alg": "ES256",
            "typ": "dpop+jwt",
            "jwk": { "kty": "EC", "crv": "P-256", "x": self.x, "y": self.y }
        });
        let payload = json!({
            "htm": "POST",
            "htu": url,
            "iat": now.timestamp(),
            "jti": uuid::Uuid::new_v4().simple().to_string()
        });
        let header_segment = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
        let payload_segment = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let signing_input = format!("{header_segment}.{payload_segment}");
        let key = SigningKey::from_slice(self.scalar.as_ref()).map_err(|_| {
            GatewayError::new(
                ErrorKind::AuthInvalid,
                "stored CodeArts DPoP key is not a valid P-256 private key",
            )
        })?;
        let signature: Signature = key.sign(signing_input.as_bytes());
        let signature_segment = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        Ok(format!("{signing_input}.{signature_segment}"))
    }

    /// Generate a P-256 keypair and its private JWK for browser authorization.
    /// The private JWK is stored encrypted and reused for token refresh.
    pub(crate) fn generate() -> Result<(Self, Value), GatewayError> {
        let signing_key = SigningKey::random(&mut rand::thread_rng());
        let scalar = signing_key.to_bytes();
        let point = signing_key.verifying_key().to_encoded_point(false);
        let x = URL_SAFE_NO_PAD.encode(
            point
                .x()
                .ok_or_else(|| GatewayError::internal("generated DPoP key has no x coordinate"))?,
        );
        let y = URL_SAFE_NO_PAD.encode(
            point
                .y()
                .ok_or_else(|| GatewayError::internal("generated DPoP key has no y coordinate"))?,
        );
        let d = URL_SAFE_NO_PAD.encode(scalar);
        let jwk = json!({"kty": "EC", "crv": "P-256", "x": x, "y": y, "d": d});
        let key = Self::from_json(&jwk)
            .ok_or_else(|| GatewayError::internal("generated DPoP key is invalid"))?;
        Ok((key, jwk))
    }
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn classify_refresh_failure(status: u16, body: &str) -> GatewayError {
    let lowered = body.to_ascii_lowercase();
    let terminal = [
        "invalid refresh token",
        "the refresh token has been used",
        "invalid client id",
        "invaliddpopheader",
        "invalid_grant",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));
    if terminal {
        return GatewayError::new(
            ErrorKind::AuthInvalid,
            "CodeArts refresh token can no longer be used; re-login is required",
        );
    }
    if status == 429 {
        return GatewayError::new(
            ErrorKind::RateLimited,
            "CodeArts token endpoint is rate limited",
        );
    }
    if (500..=599).contains(&status) {
        return GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("CodeArts token endpoint HTTP {status}"),
        );
    }
    GatewayError::new(
        ErrorKind::AuthExpired,
        format!("CodeArts token refresh failed with HTTP {status}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential_json(expiration: Option<&str>) -> Value {
        let mut value = json!({
            "access_key_id": "ak",
            "secret_access_key": "sk",
            "security_token": "sts",
            "domain_id": "domain",
        });
        if let Some(expiration) = expiration {
            value["expiration"] = json!(expiration);
        }
        value
    }

    #[test]
    fn expiry_is_only_known_when_reported() {
        let unknown = CodeArtsCredential::parse(&Credential::from_value(credential_json(None)))
            .expect("credential");
        assert!(!unknown.expired(Utc::now()));
        assert!(!unknown.expiring(Utc::now()));

        let past = CodeArtsCredential::parse(&Credential::from_value(credential_json(Some(
            "2020-01-01T00:00:00Z",
        ))))
        .expect("credential");
        assert!(past.expired(Utc::now()));
        assert!(past.expiring(Utc::now()));
    }

    #[test]
    fn refresh_requires_both_token_and_key() {
        let credential = CodeArtsCredential::parse(&Credential::from_value(credential_json(None)))
            .expect("cred");
        assert!(!credential.can_refresh());
        let debug = format!("{credential:?}");
        assert!(!debug.contains("sk"));
        assert!(!debug.contains("sts"));
    }

    #[test]
    fn dpop_key_parses_p256_jwk() {
        let key = DpopKey::from_json(&json!({
            "kty": "EC",
            "crv": "P-256",
            "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU",
            "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0",
            "d": "jpsQnnGQmL-YBIffH1136c6hXg4vJH6sV3aA8r3g3p0"
        }))
        .expect("key");
        let proof = key
            .proof("https://sts.example/v1/oauth2/tokens", Utc::now())
            .expect("proof");
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(parts[2]).expect("signature").len(),
            64
        );
    }
}
