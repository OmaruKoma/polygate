//! Qoder device-code authorization and token refresh.
//!
//! Source: Coding2API `src/provider/qoder/auth.py` and `client.py` at commit
//! `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! Flow:
//! 1. `start` generates PKCE(S256), a realm-shaped nonce and a per-flow
//!    machine id; builds `{website}/device/selectAccounts?...` locally.
//! 2. The administrator authenticates in a browser; no redirect URI is sent.
//! 3. `poll` calls `GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=`.
//!    `202`/`404` mean "waiting", `200` with a token means success. A 200 reply
//!    without a token is also treated as waiting.
//! 4. Userinfo is best effort; a credential without a stable uid is rejected.
//!
//! Refresh is `POST /api/v1/deviceToken/refresh` with only the refresh token in
//! the body (no Authorization header). Callers must serialize refresh per
//! account with [`QoderRefreshLocks`] and reload the credential after taking the
//! lock; this module does not maintain a credential store.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use parking_lot::Mutex;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::GatewayError;

use super::credential::QoderCredential;
use super::error::{
    classify_device_poll, classify_http_status, classify_transport, DevicePollOutcome,
};
use super::region;

const MAX_BODY_BYTES: usize = 128 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const PKCE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Secrets that must be stored encrypted for the lifetime of a flow.
#[derive(Clone, Serialize, Deserialize)]
pub struct QoderAuthSecrets {
    pub verifier: String,
    pub nonce: String,
    pub machine_id: String,
}

impl std::fmt::Debug for QoderAuthSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderAuthSecrets")
            .field("verifier", &"<redacted>")
            .field("nonce", &self.nonce)
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

impl Drop for QoderAuthSecrets {
    fn drop(&mut self) {
        self.verifier.zeroize();
    }
}

#[derive(Debug)]
pub struct QoderAuthStart {
    pub authorize_url: String,
    pub secrets: QoderAuthSecrets,
}

#[derive(Debug)]
pub enum QoderPollOutcome {
    Pending,
    Completed(Box<QoderCredential>),
}

/// Stateless Qoder auth/refresh HTTP client.
///
/// The base URL overrides exist for deterministic tests and for a future
/// explicit grayscale setting. They are not read from production config, so a
/// CN flow can never accidentally use an Intl override unless a caller
/// deliberately constructs this type with one.
#[derive(Clone)]
pub struct QoderAuthClient {
    http: reqwest::Client,
    openapi_override: Option<String>,
    website_override: Option<String>,
}

impl std::fmt::Debug for QoderAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderAuthClient")
            .field("openapi_override", &self.openapi_override.is_some())
            .field("website_override", &self.website_override.is_some())
            .finish()
    }
}

impl QoderAuthClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            openapi_override: None,
            website_override: None,
        }
    }

    /// Deterministic tests only. Both URLs are used verbatim for either realm.
    pub fn with_base_urls(
        http: reqwest::Client,
        openapi: impl Into<String>,
        website: impl Into<String>,
    ) -> Self {
        Self {
            http,
            openapi_override: Some(openapi.into()),
            website_override: Some(website.into()),
        }
    }

    fn openapi(&self, region: Region) -> &str {
        self.openapi_override
            .as_deref()
            .unwrap_or(region::realm(region).openapi_base)
    }

    fn website(&self, region: Region) -> &str {
        self.website_override
            .as_deref()
            .unwrap_or(region::realm(region).website)
    }

    /// Build the device authorization URL. No upstream request is made.
    pub fn start(&self, region: Region) -> Result<QoderAuthStart, GatewayError> {
        let realm = region::realm(region);
        let (verifier, challenge) = pkce_pair();
        let nonce = realm.new_nonce();
        let machine_id = uuid::Uuid::new_v4().to_string();

        let mut url = Url::parse(&format!(
            "{}/device/selectAccounts",
            self.website(region).trim_end_matches('/')
        ))
        .map_err(|_| GatewayError::internal("invalid Qoder website URL"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("challenge", &challenge);
            query.append_pair("challenge_method", "S256");
            query.append_pair("nonce", &nonce);
            if realm.send_redirect_uri {
                query.append_pair("redirect_uri", realm.redirect_uri);
            }
            query.append_pair("client_id", realm.client_id);
            query.append_pair("machine_id", &machine_id);
        }

        Ok(QoderAuthStart {
            authorize_url: url.to_string(),
            secrets: QoderAuthSecrets {
                verifier,
                nonce,
                machine_id,
            },
        })
    }

    /// Poll the device-token endpoint once.
    pub async fn poll(
        &self,
        region: Region,
        secrets: &QoderAuthSecrets,
    ) -> Result<QoderPollOutcome, GatewayError> {
        let mut url = Url::parse(&format!(
            "{}{}",
            self.openapi(region).trim_end_matches('/'),
            region::DEVICE_POLL_PATH
        ))
        .map_err(|_| GatewayError::internal("invalid Qoder OpenAPI URL"))?;
        url.query_pairs_mut()
            .append_pair("nonce", &secrets.nonce)
            .append_pair("verifier", &secrets.verifier)
            .append_pair("challenge_method", "S256");

        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, super::cosy::CLIENT_UA)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = read_body(response).await?;

        match classify_device_poll(status, &body) {
            DevicePollOutcome::Pending => Ok(QoderPollOutcome::Pending),
            DevicePollOutcome::Failed(error) => Err(error),
            DevicePollOutcome::Ready => {
                let device: Value = serde_json::from_str(&body).map_err(|_| {
                    GatewayError::upstream_server(
                        "Qoder device poll returned an unparsable response",
                    )
                })?;
                if let Some(error) = super::error::classify_payload(&device) {
                    return Err(error);
                }
                let token = string_field(&device, &["token", "device_token", "access_token"]);
                if token.is_empty() {
                    // Authorization completed but the token is not ready yet.
                    return Ok(QoderPollOutcome::Pending);
                }
                let userinfo = self.fetch_userinfo(region, &token).await;
                let credential = QoderCredential::from_device_at(
                    region,
                    &device,
                    &userinfo,
                    Utc::now().timestamp(),
                )?;
                Ok(QoderPollOutcome::Completed(Box::new(credential)))
            }
        }
    }

    async fn fetch_userinfo(&self, region: Region, access_token: &str) -> Value {
        let url = format!(
            "{}{}",
            self.openapi(region).trim_end_matches('/'),
            region::USERINFO_PATH
        );
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, super::cosy::CLIENT_UA)
            .bearer_auth(access_token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await;
        let Ok(response) = response else {
            return json!({});
        };
        if !response.status().is_success() {
            return json!({});
        }
        match read_body(response).await {
            Ok(body) => serde_json::from_str(&body).unwrap_or_else(|_| json!({})),
            Err(_) => json!({}),
        }
    }

    /// Refresh an OAuth credential. The caller must hold the per-account lock.
    pub async fn refresh(
        &self,
        region: Region,
        credential: &QoderCredential,
    ) -> Result<QoderCredential, GatewayError> {
        if credential.refresh_token.is_empty() {
            return Err(GatewayError::auth_invalid(
                "Qoder credential has no refresh token",
            ));
        }
        let url = format!(
            "{}{}",
            self.openapi(region).trim_end_matches('/'),
            region::DEVICE_REFRESH_PATH
        );
        let response = self
            .http
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, super::cosy::CLIENT_UA)
            // The refresh endpoint authenticates with the refresh token only;
            // sending an expired access token makes the upstream reject first.
            .json(&json!({"refresh_token": credential.refresh_token}))
            .timeout(REFRESH_TIMEOUT)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = read_body(response).await?;
        if status >= 400 {
            return Err(classify_http_status(status, &body));
        }
        let data: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("Qoder refresh returned an unparsable response")
        })?;
        if let Some(error) = super::error::classify_payload(&data) {
            return Err(error);
        }
        if string_field(&data, &["token", "device_token", "access_token"]).is_empty() {
            return Err(GatewayError::upstream_server(
                "Qoder refresh returned no access token",
            ));
        }
        let refreshed = credential.merge_refreshed(&data, Utc::now().timestamp());
        Ok(refreshed)
    }
}

/// Serializes refresh per account. Callers must reload the credential after
/// acquiring the lock and re-check `needs_refresh`; otherwise a queued caller
/// would refresh again with the same token family.
#[derive(Clone, Default)]
pub struct QoderRefreshLocks {
    inner: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl QoderRefreshLocks {
    pub async fn lock(&self, account_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.inner.lock();
            Arc::clone(
                map.entry(account_id.to_string())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        lock.lock_owned().await
    }
}

fn pkce_pair() -> (String, String) {
    let mut random = [0u8; 64];
    OsRng.fill_bytes(&mut random);
    let verifier: String = random
        .iter()
        .map(|byte| PKCE_ALPHABET[*byte as usize % PKCE_ALPHABET.len()] as char)
        .collect();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

async fn read_body(response: reqwest::Response) -> Result<String, GatewayError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| classify_transport(&error))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(GatewayError::upstream_server(
            "Qoder response exceeded the gateway limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn string_field(data: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            data.get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::error::ErrorKind;
    use crate::test_support::{spawn_server, Reply};

    #[test]
    fn start_builds_device_url_without_redirect_uri() {
        let client = QoderAuthClient::new(reqwest::Client::new());
        let start = client.start(Region::Cn).unwrap();
        let url = Url::parse(&start.authorize_url).unwrap();
        assert_eq!(url.path(), "/device/selectAccounts");
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(
            query.get("challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(query.contains_key("challenge"));
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some(region::CN.client_id)
        );
        assert!(query.contains_key("machine_id"));
        assert!(!query.contains_key("redirect_uri"));
        assert_eq!(start.secrets.verifier.len(), 64);
        assert_eq!(start.secrets.nonce.len(), 36);
    }

    #[test]
    fn intl_uses_hex_nonce_and_no_redirect_uri() {
        let client = QoderAuthClient::new(reqwest::Client::new());
        let start = client.start(Region::Intl).unwrap();
        assert_eq!(start.secrets.nonce.len(), 32);
        assert!(start.secrets.nonce.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(!start.authorize_url.contains("qoder.cn/device"));
        assert!(!start.authorize_url.contains("redirect_uri"));
    }

    #[tokio::test]
    async fn poll_pending_then_success_and_userinfo() {
        let polls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&polls);
        let server = spawn_server(
            move |request| match (request.method.as_str(), request.path()) {
                ("GET", "/api/v1/deviceToken/poll") => {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        Reply::json(202, "{}")
                    } else {
                        Reply::json(
                            200,
                            json!({
                                "token": "dt-access",
                                "refresh_token": "drt-refresh",
                                "expires_in": 3_600_000,
                                "user_id": "uid-1"
                            })
                            .to_string(),
                        )
                    }
                }
                ("GET", "/api/v1/userinfo") => Reply::json(
                    200,
                    json!({"id":"uid-1","name":"Alice","user_type":"pro"}).to_string(),
                ),
                _ => Reply::json(404, "{}"),
            },
        )
        .await;
        let client = QoderAuthClient::with_base_urls(
            reqwest::Client::new(),
            server.base_url.clone(),
            server.base_url.clone(),
        );
        let start = client.start(Region::Cn).unwrap();
        assert!(matches!(
            client.poll(Region::Cn, &start.secrets).await.unwrap(),
            QoderPollOutcome::Pending
        ));
        let credential = match client.poll(Region::Cn, &start.secrets).await.unwrap() {
            QoderPollOutcome::Completed(credential) => credential,
            QoderPollOutcome::Pending => panic!("expected a credential"),
        };
        assert_eq!(credential.access_token, "dt-access");
        assert_eq!(credential.nickname, "Alice");
        assert_eq!(credential.user_type, "pro");

        let requests = server.requests();
        let poll = requests
            .iter()
            .find(|request| request.path() == "/api/v1/deviceToken/poll")
            .unwrap();
        assert!(poll.target.contains("nonce="));
        assert!(poll.target.contains("verifier="));
        let userinfo = requests
            .iter()
            .find(|request| request.path() == "/api/v1/userinfo")
            .unwrap();
        assert_eq!(userinfo.header("authorization"), Some("Bearer dt-access"));
    }

    #[tokio::test]
    async fn poll_rejects_a_login_without_uid() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("GET", "/api/v1/deviceToken/poll") => {
                Reply::json(200, json!({"token":"dt-access"}).to_string())
            }
            _ => Reply::json(500, "{}"),
        })
        .await;
        let client = QoderAuthClient::with_base_urls(
            reqwest::Client::new(),
            server.base_url.clone(),
            server.base_url.clone(),
        );
        let start = client.start(Region::Cn).unwrap();
        let error = client.poll(Region::Cn, &start.secrets).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[tokio::test]
    async fn refresh_uses_only_the_refresh_token() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("POST", "/api/v1/deviceToken/refresh") => Reply::json(
                200,
                json!({"token":"dt-new","expires_in":7_200_000}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = QoderAuthClient::with_base_urls(
            reqwest::Client::new(),
            server.base_url.clone(),
            server.base_url.clone(),
        );
        let credential = QoderCredential::from_device_at(
            Region::Cn,
            &json!({"token":"dt-old","refresh_token":"drt-old","user_id":"uid-1"}),
            &json!({"id":"uid-1","name":"Alice"}),
            1_000,
        )
        .unwrap();
        let refreshed = client.refresh(Region::Cn, &credential).await.unwrap();
        assert_eq!(refreshed.access_token, "dt-new");
        assert_eq!(refreshed.uid, "uid-1");
        assert_eq!(refreshed.nickname, "Alice");
        assert_eq!(refreshed.refresh_token, "drt-old");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].header("authorization").is_none());
        assert!(requests[0].body.contains("drt-old"));
    }

    #[tokio::test]
    async fn refresh_without_token_is_rejected_before_network() {
        let client = QoderAuthClient::new(reqwest::Client::new());
        let credential = QoderCredential::from_device_at(
            Region::Cn,
            &json!({"token":"dt-old","user_id":"uid-1"}),
            &json!({}),
            1_000,
        )
        .unwrap();
        let error = client.refresh(Region::Cn, &credential).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[tokio::test]
    async fn refresh_locks_serialize_per_account() {
        let locks = Arc::new(QoderRefreshLocks::default());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let locks = Arc::clone(&locks);
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            tasks.push(tokio::spawn(async move {
                let _guard = locks.lock("acct-1").await;
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1, "holders must be exclusive");

        let first = locks.lock("acct-a").await;
        let second =
            tokio::time::timeout(Duration::from_millis(50), Arc::clone(&locks).lock("acct-b"))
                .await;
        drop(first);
        assert!(
            second.is_ok(),
            "different accounts must not block each other"
        );
    }
}
