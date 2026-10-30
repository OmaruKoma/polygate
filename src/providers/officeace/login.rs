//! OfficeAce browser authorization: PKCE + DPoP + cloud-handled callback.
//!
//! Flow (every hop measured by the reference implementation):
//!
//! 1. `POST {cloud}/v1/claw/auth/state` — the state is issued by the cloud; a
//!    locally invented one answers `AgentArts.11000004`;
//! 2. generate a PKCE verifier, its SHA-256 challenge and a fresh P-256 DPoP key;
//! 3. the operator authorizes on the Huawei cloud page; the callback is handled
//!    by the cloud, so there is no local port and no pasted URL;
//! 4. `GET {cloud}/v1/claw/auth/code?state=…` — `202` while pending;
//! 5. `POST {sts}/v1/oauth2/tokens` — authorization code + PKCE + DPoP proof;
//! 6. `GET {cloud}/v1/claw/client-permission-validate` — app key and the
//!    per-account model gateway.
//!
//! The DPoP key stays with the refresh token for the lifetime of the account:
//! STS binds the token to the key that requested it, and a new key answers
//! `InvalidDPoPHeader`. `client_id` is `pdp5_for_agentarts`, which is **not**
//! interchangeable with `codearts-agent` (that answers `STS5.1806`).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::error::{ErrorKind, GatewayError};
use crate::providers::codearts::auth::DpopKey;

use super::http::OfficeAceHttp;
use super::region;
use super::signer::SignCredential;

/// Parameters of the login flow. Defaults are the production values.
#[derive(Debug, Clone)]
pub struct LoginConfig {
    pub client_id: String,
    pub cloud: String,
    pub auth_base: String,
    pub sts: String,
    pub switch_account: bool,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            client_id: region::CLIENT_ID.to_string(),
            cloud: region::CLOUD_HOST.to_string(),
            auth_base: region::AUTH_HOST.to_string(),
            sts: region::STS_HOST.to_string(),
            switch_account: false,
        }
    }
}

impl LoginConfig {
    pub fn redirect_uri(&self) -> String {
        format!(
            "{}{}",
            self.cloud.trim_end_matches('/'),
            region::EP_REDIRECT_PATH
        )
    }
}

/// Secret material kept encrypted while the flow is pending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginSecrets {
    pub state: String,
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
    pub authorize_url: String,
    pub secrets: LoginSecrets,
}

/// Result of one poll of the cloud authorization-code endpoint.
pub enum PollOutcome {
    Pending,
    Completed(Box<LoginOutcome>),
}

#[derive(Debug)]
pub struct LoginOutcome {
    pub credential: Value,
    pub account_id: String,
    pub nickname: String,
}

/// Ask the cloud for a state and build the authorize URL. No token is minted.
pub async fn begin(http: &OfficeAceHttp, config: &LoginConfig) -> Result<LoginStart, GatewayError> {
    let state = http.auth_state().await?;
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let (_key, jwk) = DpopKey::generate()?;
    let authorize_url = authorize_url(config, &state, &challenge);
    let authorize_url = if config.switch_account {
        switch_account_url(config, &authorize_url)
    } else {
        authorize_url
    };
    Ok(LoginStart {
        authorize_url,
        secrets: LoginSecrets {
            state,
            code_verifier: verifier,
            dpop_private_jwk: jwk,
        },
    })
}

/// One poll. `Pending` means the operator has not finished authorizing yet.
pub async fn poll(
    http: &OfficeAceHttp,
    config: &LoginConfig,
    secrets: &LoginSecrets,
) -> Result<PollOutcome, GatewayError> {
    let Some(code) = http.auth_code(&secrets.state).await? else {
        return Ok(PollOutcome::Pending);
    };
    let outcome = complete(http, config, &code, secrets).await?;
    Ok(PollOutcome::Completed(Box::new(outcome)))
}

/// Exchange the code, then validate that the account is provisioned.
pub async fn complete(
    http: &OfficeAceHttp,
    config: &LoginConfig,
    code: &str,
    secrets: &LoginSecrets,
) -> Result<LoginOutcome, GatewayError> {
    let dpop = DpopKey::from_json(&secrets.dpop_private_jwk).ok_or_else(|| {
        GatewayError::internal("stored OfficeAce login secrets have an invalid DPoP key")
    })?;
    let token_url = format!(
        "{}{}",
        http.sts().trim_end_matches('/'),
        region::EP_OAUTH_TOKENS
    );
    let proof = dpop.proof(&token_url, Utc::now())?;
    let form = vec![
        ("client_id", config.client_id.clone()),
        ("code", code.to_string()),
        ("code_verifier", secrets.code_verifier.clone()),
        ("grant_type", "authorization_code".to_string()),
        ("redirect_uri", config.redirect_uri()),
    ];
    let tokens = http.token_request(&form, proof).await?;
    let credential = credential_from_tokens(&tokens, config, &secrets.dpop_private_jwk)?;
    // The chat app key only arrives from `client-permission-validate`, so the
    // call that fetches it is signed with the temporary AK/SK alone. Parsing a
    // full credential here would reject every first-time login.
    let info = http
        .permission_validate(&sign_credential_from_tokens(&credential)?, Utc::now())
        .await?;
    provision(&credential, &info)
}

fn sign_credential_from_tokens(credential: &Value) -> Result<SignCredential, GatewayError> {
    let field = |key: &str| {
        credential
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let access_key_id = field("access_key_id");
    let secret_access_key = field("secret_access_key");
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return Err(GatewayError::upstream_server(
            "OfficeAce token response has no access key to validate the subscription",
        ));
    }
    Ok(SignCredential {
        access_key_id,
        secret_access_key,
        security_token: field("security_token"),
        project_id: field("project_id"),
    })
}

/// `client-permission-validate` → `{account, app_key, secret, gateway, name}`.
fn provision(credential: &Value, info: &Value) -> Result<LoginOutcome, GatewayError> {
    let subscription = info.get("subscription").filter(|value| value.is_object());
    let model_info = [
        info.get("model_info"),
        subscription.and_then(|s| s.get("model_info")),
    ]
    .into_iter()
    .flatten()
    .find(|value| value.as_object().is_some_and(|object| !object.is_empty()))
    .cloned()
    .unwrap_or(Value::Null);
    let auth_info = model_info
        .get("model_auth_info")
        .cloned()
        .unwrap_or(Value::Null);
    let app_key = auth_info
        .get("model_app_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let app_secret = auth_info
        .get("model_app_secret")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let base_url = normalized_base(
        model_info
            .get("model_api_url_base")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let account_id = text(info, "account_id");
    let principal_id = text(info, "principal_id");
    let user_name = user_name_of(info);

    if app_key.is_empty() || app_secret.is_empty() {
        let who = if !user_name.is_empty() {
            user_name.clone()
        } else {
            account_id.clone()
        };
        let who = if who.is_empty() {
            "这个账号".to_string()
        } else {
            format!("华为云账号 {who}")
        };
        return Err(GatewayError::new(
            ErrorKind::AuthInvalid,
            format!(
                "{who} 还没有开通 OfficeAce 服务：请先在官方客户端登录并兑换邀请码完成开通，或者用已开通的账号重新登录"
            ),
        ));
    }
    if base_url.is_empty() {
        return Err(GatewayError::upstream_server(
            "OfficeAce permission validate response has no model gateway address",
        ));
    }

    let mut updated = credential.clone();
    if let Some(object) = updated.as_object_mut() {
        object.insert("model_app_key".into(), json!(app_key));
        object.insert("model_app_secret".into(), json!(app_secret));
        object.insert("model_base_url".into(), json!(base_url));
        if !account_id.is_empty() {
            object.insert("account_id".into(), json!(account_id));
        }
        if !principal_id.is_empty() {
            object.insert("principal_id".into(), json!(principal_id));
        }
        if !user_name.is_empty() {
            object.insert("user_name".into(), json!(user_name));
        }
    }
    let nickname = {
        let value = text(&updated, "nickname");
        if !value.is_empty() {
            value
        } else if !user_name.is_empty() {
            user_name
        } else {
            account_id.clone()
        }
    };
    Ok(LoginOutcome {
        credential: updated,
        account_id,
        nickname,
    })
}

fn credential_from_tokens(
    tokens: &Value,
    config: &LoginConfig,
    dpop_jwk: &Value,
) -> Result<Value, GatewayError> {
    let credentials = tokens.get("credentials").ok_or_else(|| {
        GatewayError::upstream_server("OfficeAce token response is missing credentials")
    })?;
    let required = |key: &str| -> Result<String, GatewayError> {
        credentials
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                GatewayError::upstream_server(format!("OfficeAce token response is missing {key}"))
            })
    };
    Ok(json!({
        "account_id": text(tokens, "user_id"),
        "principal_id": text(tokens, "principal_id"),
        "user_name": id_token_name(tokens.get("id_token").and_then(Value::as_str).unwrap_or_default()),
        "access_key_id": required("access_key_id")?,
        "secret_access_key": required("secret_access_key")?,
        "security_token": required("security_token")?,
        "project_id": credentials
            .get("project_id")
            .and_then(Value::as_str)
            .or_else(|| tokens.get("project_id").and_then(Value::as_str))
            .unwrap_or_default(),
        "expiration": credentials
            .get("expiration")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "refresh_token": tokens
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "client_id": config.client_id,
        "dpop_private_jwk": dpop_jwk,
    }))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn user_name_of(info: &Value) -> String {
    let explicit = text(info, "user_name");
    if !explicit.is_empty() {
        return explicit;
    }
    // The endpoint has no user-name field; it appears at the end of the URN
    // (`iam::<account>:user:<name>`). Missing it is legal.
    let urn = text(info, "principal_urn");
    urn.split_once(":user:")
        .map(|(_, name)| name.trim().to_string())
        .unwrap_or_default()
}

fn normalized_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    region::normalize_gateway(&with_scheme)
}

fn id_token_name(id_token: &str) -> String {
    let mut parts = id_token.split('.');
    let Some(payload) = parts.nth(1) else {
        return String::new();
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return String::new();
    };
    ["preferred_username", "name"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn pkce_verifier() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn authorize_url(config: &LoginConfig, state: &str, challenge: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &config.client_id)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "SHA-256")
        .append_pair("state", state)
        .append_pair("scope", "openid")
        .append_pair("redirect_uri", &config.redirect_uri())
        .append_pair("response_type", "code")
        .finish();
    let inner = format!(
        "{}{}?{}",
        config.auth_base.trim_end_matches('/'),
        region::EP_AUTHORIZE,
        query
    );
    // The login page wrapper is required: without a session the authorize URL
    // redirects to the login page and loses the return address.
    format!(
        "{}{}?service={}",
        config.auth_base.trim_end_matches('/'),
        region::EP_LOGIN_PAGE,
        url::form_urlencoded::byte_serialize(inner.as_bytes()).collect::<String>()
    )
}

fn switch_account_url(config: &LoginConfig, authorize_url: &str) -> String {
    format!(
        "{}{}?service={}",
        config.auth_base.trim_end_matches('/'),
        region::EP_LOGOUT,
        url::form_urlencoded::byte_serialize(authorize_url.as_bytes()).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn the_authorize_url_wraps_the_login_page_and_carries_pkce() {
        let config = LoginConfig::default();
        let url = authorize_url(&config, "state-1", "challenge-1");
        assert!(url.starts_with("https://auth.huaweicloud.com/authui/login.html?service="));
        let parsed = url::Url::parse(&url).expect("login url");
        let decoded = parsed
            .query_pairs()
            .find(|(key, _)| key == "service")
            .map(|(_, value)| value.to_string())
            .expect("service value");
        assert!(decoded.contains("code_challenge=challenge-1"));
        assert!(decoded.contains("client_id=pdp5_for_agentarts"));
        assert!(decoded.contains("state=state-1"));
    }

    #[test]
    fn the_switch_account_url_logs_out_first() {
        let config = LoginConfig::default();
        let url = switch_account_url(&config, "https://auth.example/a");
        assert!(url.starts_with("https://auth.huaweicloud.com/authui/logout?service="));
    }

    #[test]
    fn pkce_challenge_is_the_sha256_of_the_verifier() {
        let verifier = "verifier-value";
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(pkce_challenge(verifier), expected);
    }

    #[test]
    fn a_reported_gateway_with_a_v2_suffix_is_normalized() {
        assert_eq!(
            normalized_base("https://gw.example/v2"),
            "https://gw.example"
        );
        assert_eq!(normalized_base("gw.example"), "https://gw.example");
    }

    #[test]
    fn an_unprovisioned_account_names_the_problem() {
        let credential = json!({"access_key_id": "a", "secret_access_key": "b"});
        let error = provision(
            &credential,
            &json!({"subscription": null, "account_id": "acc-1"}),
        )
        .expect_err("unprovisioned");
        assert!(error.message.contains("还没有开通 OfficeAce"));
    }

    #[test]
    fn a_provisioned_account_receives_a_complete_credential() {
        let credential = json!({
            "access_key_id": "a", "secret_access_key": "b", "security_token": "c",
            "refresh_token": "r", "client_id": "pdp5_for_agentarts",
            "dpop_private_jwk": {"kty": "EC"},
        });
        let outcome = provision(
            &credential,
            &json!({
                "account_id": "acc-1",
                "principal_urn": "iam::1:user:alice",
                "subscription": {"model_info": {
                    "model_api_url_base": "https://gw.example/v2",
                    "model_auth_info": {"model_app_key": "k", "model_app_secret": "s"}
                }}
            }),
        )
        .expect("provisioned");
        assert_eq!(outcome.account_id, "acc-1");
        assert_eq!(outcome.nickname, "alice");
        assert_eq!(
            outcome.credential["model_base_url"],
            json!("https://gw.example")
        );
        assert_eq!(outcome.credential["model_app_key"], json!("k"));
    }
}
