//! CodeArts browser authorization (authorization code + PKCE + DPoP).
//!
//! Protocol evidence: local `codearts2api` `internal/auth/manager.go`
//! (`BeginLoginSession`, `ExchangeCode`, `tokenRequest`, `queryIdentity`) and
//! `internal/auth/dpop.go`. The portal returns only `code` to the registered
//! callback, so PKCE plus a single pending flow is the binding. DPoP uses a
//! fresh P-256 key per flow and must be reused for refresh.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use crate::accounts::Credential;
use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};

use super::auth::DpopKey;
use super::signer::{sign_request, SignCredential};

const TOKEN_PATH: &str = "/v1/oauth2/tokens";
const IDENTITY_PATH: &str = "/v5/caller-identity";
const MAX_ERROR_CODE: usize = 64;
const MAX_ERROR_DESCRIPTION: usize = 160;

/// Parameters copied from the source implementation's configuration.
#[derive(Debug, Clone)]
pub struct LoginConfig {
    pub client_id: String,
    pub uri_scheme: String,
    pub theme: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub locale: String,
    pub cn_portal_url: String,
    pub intl_portal_url: String,
    pub sts_url: String,
}

/// Secret material that must be stored encrypted until the callback arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginSecrets {
    pub code_verifier: String,
    pub dpop_private_jwk: Value,
}

impl Drop for LoginSecrets {
    fn drop(&mut self) {
        self.code_verifier.zeroize();
        zeroize_value(&mut self.dpop_private_jwk);
    }
}

fn zeroize_value(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(zeroize_value),
        Value::Object(map) => map.values_mut().for_each(zeroize_value),
        _ => {}
    }
}

#[derive(Debug)]
pub struct LoginStart {
    pub ticket_id: String,
    pub authorize_url: String,
    pub secrets: LoginSecrets,
}

#[derive(Debug)]
pub struct LoginOutcome {
    pub credential: Credential,
    pub domain_id: String,
    pub principal_id: String,
    pub name: String,
    pub metadata: Value,
}

#[derive(Deserialize)]
struct TokenPayload {
    #[serde(default)]
    credentials: TokenCredentials,
    #[serde(default)]
    refresh_token: String,
}

#[derive(Default, Deserialize)]
struct TokenCredentials {
    #[serde(default)]
    access_key_id: String,
    #[serde(default)]
    secret_access_key: String,
    #[serde(default)]
    security_token: String,
    #[serde(default)]
    expiration: String,
}

#[derive(Deserialize)]
struct IdentityPayload {
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    principal_id: String,
    #[serde(default)]
    principal_urn: String,
}

/// Build the portal URL and fresh PKCE/DPoP material. No network call.
pub fn begin(
    config: &LoginConfig,
    region: Region,
    redirect_uri: &str,
    port: u16,
    now: DateTime<Utc>,
) -> Result<LoginStart, GatewayError> {
    let _ = now;
    let mut verifier_bytes = [0u8; 64];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut verifier_bytes);
    let code_verifier = hex::encode(verifier_bytes);
    verifier_bytes.zeroize();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let (_, dpop_private_jwk) = DpopKey::generate()?;
    let ticket_id = Uuid::new_v4().to_string();

    let portal = match region {
        Region::Cn => config.cn_portal_url.as_str(),
        Region::Intl => config.intl_portal_url.as_str(),
    };
    let mut url = Url::parse(&format!(
        "{}/portal/authorize",
        portal.trim_end_matches('/')
    ))
    .map_err(|_| GatewayError::config("invalid CodeArts portal URL"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("theme", &config.theme)
            .append_pair("locale", &config.locale)
            .append_pair("uri_scheme", &config.uri_scheme)
            .append_pair("client_id", &config.client_id)
            .append_pair("port", &port.to_string())
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "SHA-256")
            .append_pair("ticket_id", &ticket_id)
            .append_pair("auth_callback_url", redirect_uri)
            .append_pair("plugin-name", &config.plugin_name)
            .append_pair("plugin-version", &config.plugin_version);
        if region == Region::Intl {
            query.append_pair("quickcompfalg", "1");
        }
    }

    Ok(LoginStart {
        ticket_id,
        authorize_url: url.to_string(),
        secrets: LoginSecrets {
            code_verifier,
            dpop_private_jwk,
        },
    })
}

/// Exchange the authorization code, then read the caller identity with the
/// returned AK/SK. Both requests are required before an account exists.
pub async fn exchange(
    http: &reqwest::Client,
    config: &LoginConfig,
    redirect_uri: &str,
    code: &str,
    secrets: &LoginSecrets,
    now: DateTime<Utc>,
) -> Result<LoginOutcome, GatewayError> {
    let code = code.trim();
    if code.is_empty() || code.len() > 4096 {
        return Err(GatewayError::auth_invalid(
            "CodeArts callback did not include a usable code",
        ));
    }
    let dpop = DpopKey::from_json(&secrets.dpop_private_jwk)
        .ok_or_else(|| GatewayError::internal("pending authorization has an invalid DPoP key"))?;
    let token_url = format!("{}{TOKEN_PATH}", config.sts_url.trim_end_matches('/'));
    let proof = dpop.proof(&token_url, now)?;

    let response = http
        .post(&token_url)
        .header("DPoP", proof)
        .form(&[
            ("client_id", config.client_id.clone()),
            ("code", code.to_string()),
            ("code_verifier", secrets.code_verifier.clone()),
            ("grant_type", "authorization_code".to_string()),
            ("redirect_uri", redirect_uri.to_string()),
        ])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|error| map_transport("token", error))?;

    let status = response.status().as_u16();
    let mut body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(map_token_status(status, &body));
    }
    let payload: TokenPayload = serde_json::from_str(&body).map_err(|_| {
        GatewayError::upstream_server("CodeArts token endpoint returned a non-JSON response")
    })?;
    let mut access_key_id = Zeroizing::new(payload.credentials.access_key_id);
    let mut secret_access_key = Zeroizing::new(payload.credentials.secret_access_key);
    let mut security_token = Zeroizing::new(payload.credentials.security_token);
    let mut refresh_token = Zeroizing::new(payload.refresh_token);
    let expiration = payload.credentials.expiration;
    if access_key_id.trim().is_empty() || secret_access_key.trim().is_empty() {
        return Err(GatewayError::upstream_server(
            "CodeArts token response is missing credentials",
        ));
    }
    if access_key_id.len() > 256
        || secret_access_key.len() > 4096
        || security_token.len() > 8192
        || refresh_token.len() > 8192
    {
        return Err(GatewayError::upstream_server(
            "CodeArts token response contains oversized fields",
        ));
    }

    let sign = SignCredential {
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        security_token: security_token.to_string(),
    };
    let identity = query_identity(http, &config.sts_url, &sign, now).await?;

    let mut credential = json!({
        "access_key_id": access_key_id.as_str(),
        "secret_access_key": secret_access_key.as_str(),
        "security_token": security_token.as_str(),
        "domain_id": identity.account_id,
        "code_verifier": secrets.code_verifier.clone(),
        "dpop_private_jwk": secrets.dpop_private_jwk.clone(),
    });
    if !expiration.trim().is_empty() {
        credential["expiration"] = json!(expiration);
    }
    if !refresh_token.trim().is_empty() {
        credential["refresh_token"] = json!(refresh_token.as_str());
    }
    let name = identity
        .principal_urn
        .rsplit(':')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("CodeArts account")
        .to_string();
    let metadata = json!({
        "source": "browser-authorization",
        "principal_urn": identity.principal_urn.clone(),
        "principal_id": identity.principal_id.clone(),
        "domain_id": identity.account_id.clone(),
    });

    access_key_id.zeroize();
    secret_access_key.zeroize();
    security_token.zeroize();
    refresh_token.zeroize();
    body.zeroize();

    Ok(LoginOutcome {
        credential: Credential::from_value(credential),
        domain_id: identity.account_id,
        principal_id: identity.principal_id,
        name,
        metadata,
    })
}

async fn query_identity(
    http: &reqwest::Client,
    sts_url: &str,
    sign: &SignCredential,
    now: DateTime<Utc>,
) -> Result<IdentityPayload, GatewayError> {
    let url = Url::parse(&format!("{}{IDENTITY_PATH}", sts_url.trim_end_matches('/')))
        .map_err(|_| GatewayError::config("invalid CodeArts STS URL"))?;
    let signed = sign_request(
        "GET",
        &url,
        &[("Content-Type".to_string(), "application/json".to_string())],
        b"",
        sign,
        now,
    );
    let mut request = http.get(url).timeout(std::time::Duration::from_secs(20));
    for (name, value) in signed.headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| map_transport("identity", error))?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(match status {
            401 | 403 => {
                GatewayError::auth_invalid("CodeArts rejected the newly issued credentials")
            }
            _ => GatewayError::upstream_server(format!(
                "CodeArts caller identity failed with HTTP {status}"
            )),
        });
    }
    let identity: IdentityPayload = serde_json::from_str(&body).map_err(|_| {
        GatewayError::upstream_server("CodeArts caller identity returned a non-JSON response")
    })?;
    if identity.account_id.trim().is_empty() || identity.principal_id.trim().is_empty() {
        return Err(GatewayError::upstream_server(
            "CodeArts caller identity is missing account_id or principal_id",
        ));
    }
    Ok(identity)
}

fn map_transport(stage: &'static str, error: reqwest::Error) -> GatewayError {
    use std::error::Error as _;

    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(" <- ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    tracing::warn!(
        target: "polygate::codearts::login",
        stage,
        error = %chain,
        timeout = error.is_timeout(),
        connect = error.is_connect(),
        request = error.is_request(),
        "CodeArts authorization transport failure"
    );
    if error.is_timeout() {
        GatewayError::timeout("CodeArts authorization timed out")
    } else {
        GatewayError::transport("CodeArts authorization request failed")
    }
}

fn map_token_status(status: u16, body: &str) -> GatewayError {
    let code = error_code(body);
    match status {
        429 => GatewayError::new(
            ErrorKind::RateLimited,
            "CodeArts token endpoint is rate limited",
        ),
        status if (500..=599).contains(&status) => GatewayError::upstream_server(format!(
            "CodeArts token endpoint failed with HTTP {status}"
        )),
        _ => {
            let detail = match (code, error_description(body)) {
                (Some(code), Some(desc)) => format!(" ({code}: {desc})"),
                (Some(code), None) => format!(" ({code})"),
                (None, Some(desc)) => format!(" ({desc})"),
                (None, None) => String::new(),
            };
            GatewayError::auth_invalid(format!("CodeArts authorization was rejected{detail}"))
        }
    }
}

/// Human-readable reason the token endpoint gave, when it sends one.
///
/// The authorization code itself is never part of an error body, so echoing the
/// description to the administrator is safe and is the only way to tell an
/// expired code from a `redirect_uri` mismatch or a PKCE failure.
fn error_description(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let text = ["error_description", "error_msg", "message"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))?;
    let cleaned: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_ERROR_DESCRIPTION)
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

fn error_code(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let code = value
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| value.get("error_code").and_then(Value::as_str))?;
    let code: String = code
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(MAX_ERROR_CODE)
        .collect();
    (!code.is_empty()).then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Region;

    #[test]
    fn token_rejection_keeps_the_upstream_description() {
        let body =
            r#"{"error":"STS5.1805","error_description":"The authorization code is invalid."}"#;
        let error = map_token_status(400, body);
        assert_eq!(error.kind, crate::error::ErrorKind::AuthInvalid);
        assert!(error.message.contains("STS5.1805"), "{}", error.message);
        assert!(
            error.message.contains("authorization code is invalid"),
            "{}",
            error.message
        );
    }

    #[test]
    fn token_rejection_without_body_stays_generic() {
        let error = map_token_status(400, "");
        assert!(error.message.ends_with("rejected"), "{}", error.message);
    }

    fn config() -> LoginConfig {
        LoginConfig {
            client_id: "codearts-snap".into(),
            uri_scheme: "codearts-snap".into(),
            theme: "2".into(),
            plugin_name: "codearts-snap_vscode".into(),
            plugin_version: "26.7.2".into(),
            locale: "zh-cn".into(),
            cn_portal_url: "https://codearts.huaweicloud.com".into(),
            intl_portal_url: "https://codearts.ap-southeast-1.huaweicloud.com".into(),
            sts_url: "https://sts.cn-north-4.myhuaweicloud.com".into(),
        }
    }

    #[test]
    fn begin_builds_the_documented_portal_url() {
        let start = begin(
            &config(),
            Region::Cn,
            "http://127.0.0.1:8787/oauth/callback",
            8787,
            Utc::now(),
        )
        .unwrap();
        let url = Url::parse(&start.authorize_url).unwrap();
        assert_eq!(url.path(), "/portal/authorize");
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], "codearts-snap");
        assert_eq!(params["code_challenge_method"], "SHA-256");
        assert_eq!(
            params["auth_callback_url"],
            "http://127.0.0.1:8787/oauth/callback"
        );
        assert_eq!(params["port"], "8787");
        assert!(!params.contains_key("quickcompfalg"));
        assert_eq!(start.secrets.code_verifier.len(), 128);
        assert!(start.secrets.dpop_private_jwk.get("d").is_some());
    }

    #[test]
    fn intl_authorization_sets_quickcomp_flag() {
        let start = begin(
            &config(),
            Region::Intl,
            "http://127.0.0.1:8787/oauth/callback",
            8787,
            Utc::now(),
        )
        .unwrap();
        let url = Url::parse(&start.authorize_url).unwrap();
        assert!(url
            .query_pairs()
            .any(|(key, value)| key == "quickcompfalg" && value == "1"));
    }
}
