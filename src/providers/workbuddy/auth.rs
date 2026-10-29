//! WorkBuddy device authorization (CN and Intl).
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §4.1; Coding2API
//! `src/provider/codebuddy/oauth.py` (MIT, protocol facts only):
//!
//! ```text
//! POST {chat_base}/v2/plugin/auth/state?platform=CLI   -> data.authUrl + data.state
//! user opens authUrl and authorizes on the official page
//! GET  {chat_base}/v2/plugin/auth/token?state=...      -> code 11217 = waiting
//! GET  {chat_base}/v2/plugin/login/account?state=...   -> code 12151 = account not ready
//! ```
//!
//! Scope: start and poll only. The caller owns the flow lifetime (TTL, cancel,
//! one flow at a time) and persists the returned credential through
//! `CredentialService`. There is no upstream cancel endpoint; cancellation is a
//! local decision. No device proof, no client-identity headers, and no
//! interaction on the user's behalf — see the compatibility gaps in
//! `PROGRESS.md` §37.5.

use std::time::Duration;

use chrono::Utc;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde_json::{json, Value};

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::profile::{self, Identity};

use super::credential::WorkbuddyCredential;
use super::error::{classify_http_status, classify_payload, classify_transport};
use super::region;

pub const AUTH_STATE_TTL_SECONDS: i64 = 600;
pub const POLL_INTERVAL_SECONDS: u64 = 5;
const PENDING_CODE: i64 = 11217;
const ACCOUNT_PENDING_CODE: i64 = 12151;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BODY_BYTES: usize = 256 * 1024;
const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct WorkbuddyAuthClient {
    client: reqwest::Client,
    base_override: Option<String>,
    identity: Identity,
}

impl std::fmt::Debug for WorkbuddyAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkbuddyAuthClient")
            .field("base_override", &self.base_override)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbuddyAuthStart {
    pub auth_url: String,
    /// Upstream `state`; required for every poll. Never shown to the browser as
    /// a credential, but the operator-visible flow id is a separate value.
    pub upstream_state: String,
    pub expires_at: i64,
    pub interval_seconds: u64,
}

#[derive(Debug)]
pub enum WorkbuddyPollOutcome {
    /// The user has not finished (or the account record is still being created).
    Pending,
    Completed(Box<WorkbuddyCredential>),
}

impl WorkbuddyAuthClient {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_override: None,
            identity: Identity::default(),
        }
    }

    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// Tests only: point both the auth and chat calls at one mock server.
    pub fn with_base(client: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            client,
            base_override: Some(base.into().trim_end_matches('/').to_string()),
            identity: Identity::default(),
        }
    }

    /// The device-code flow uses the plugin endpoints; only the UA differs
    /// between the honest and the impersonating mode.
    fn user_agent(&self, region: Region) -> String {
        match self.identity {
            Identity::Impersonate => profile::workbuddy_desktop_ua(region),
            Identity::Honest => CLIENT_UA.to_string(),
        }
    }

    fn base(&self, region: Region) -> &str {
        self.base_override
            .as_deref()
            .unwrap_or(region::realm(region).chat_base)
    }

    pub async fn start(&self, region: Region) -> Result<WorkbuddyAuthStart, GatewayError> {
        self.start_at(region, Utc::now().timestamp()).await
    }

    async fn start_at(&self, region: Region, now: i64) -> Result<WorkbuddyAuthStart, GatewayError> {
        let url = format!(
            "{}?platform=CLI",
            region::realm(region).url_at(self.base(region), region::AUTH_STATE_PATH)
        );
        let response = self
            .client
            .post(&url)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(USER_AGENT, self.user_agent(region))
            .json(&json!({}))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = read_body(response).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy auth state returned unparsable JSON")
        })?;
        if let Some(error) = classify_payload(&payload) {
            return Err(error);
        }
        let data = payload
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GatewayError::upstream_server("WorkBuddy auth state response has no data object")
            })?;
        let auth_url = text(data.get("authUrl"));
        let upstream_state = text(data.get("state"));
        if auth_url.is_empty() || upstream_state.is_empty() {
            return Err(GatewayError::upstream_server(
                "WorkBuddy auth state response is missing authUrl or state",
            ));
        }
        Ok(WorkbuddyAuthStart {
            auth_url,
            upstream_state,
            expires_at: now + AUTH_STATE_TTL_SECONDS,
            interval_seconds: POLL_INTERVAL_SECONDS,
        })
    }

    /// Poll once. `expires_at` is the flow deadline chosen by the caller.
    pub async fn poll(
        &self,
        region: Region,
        upstream_state: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<WorkbuddyPollOutcome, GatewayError> {
        if upstream_state.trim().is_empty() {
            return Err(GatewayError::bad_request("authorization state is empty"));
        }
        if expires_at > 0 && now >= expires_at {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "WorkBuddy device authorization expired; start it again",
            ));
        }

        let token_url = format!(
            "{}?state={}",
            region::realm(region).url_at(self.base(region), region::AUTH_TOKEN_PATH),
            encode(upstream_state)
        );
        let response = self
            .client
            .get(&token_url)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(USER_AGENT, self.user_agent(region))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = read_body(response).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy auth poll returned unparsable JSON")
        })?;
        match code_of(&payload) {
            Some(PENDING_CODE) => return Ok(WorkbuddyPollOutcome::Pending),
            Some(0) => {}
            _ => {
                return Err(classify_payload(&payload).unwrap_or_else(|| {
                    GatewayError::upstream_server("WorkBuddy auth poll was rejected")
                }))
            }
        }
        let token = payload
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GatewayError::upstream_server("WorkBuddy auth poll has no data object")
            })?;
        let access_token = text(token.get("accessToken"));
        if access_token.is_empty() {
            // `code: 0` without a token is a malformed success, not a wait
            // state; only 11217 means "still waiting".
            return Err(GatewayError::upstream_server(
                "WorkBuddy auth poll succeeded without an access token",
            ));
        }

        let account = match self
            .fetch_account(region, upstream_state, &access_token)
            .await?
        {
            Some(account) => account,
            None => return Ok(WorkbuddyPollOutcome::Pending),
        };
        Ok(WorkbuddyPollOutcome::Completed(Box::new(build_credential(
            region,
            &payload,
            token,
            &account,
            &access_token,
            now,
        )?)))
    }

    async fn fetch_account(
        &self,
        region: Region,
        upstream_state: &str,
        access_token: &str,
    ) -> Result<Option<Value>, GatewayError> {
        let url = format!(
            "{}?state={}",
            region::realm(region).url_at(self.base(region), region::LOGIN_ACCOUNT_PATH),
            encode(upstream_state)
        );
        let response = self
            .client
            .get(&url)
            .header(ACCEPT, "application/json, text/plain, */*")
            .header(USER_AGENT, self.user_agent(region))
            .header(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|_| {
                    GatewayError::auth_invalid("WorkBuddy access token cannot be sent")
                })?,
            )
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = read_body(response).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy account poll returned unparsable JSON")
        })?;
        match code_of(&payload) {
            Some(ACCOUNT_PENDING_CODE) => Ok(None),
            Some(0) => Ok(Some(
                payload
                    .get("data")
                    .and_then(Value::as_object)
                    .cloned()
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({})),
            )),
            _ => Err(classify_payload(&payload).unwrap_or_else(|| {
                GatewayError::upstream_server("WorkBuddy account poll was rejected")
            })),
        }
    }
}

fn build_credential(
    region: Region,
    payload: &Value,
    token: &serde_json::Map<String, Value>,
    account_data: &Value,
    access_token: &str,
    now: i64,
) -> Result<WorkbuddyCredential, GatewayError> {
    let account = account_data
        .get("account")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let uid = text(account.get("uid"));
    let uid = if uid.is_empty() {
        text(token.get("user_id"))
    } else {
        uid
    };
    if uid.is_empty() {
        return Err(GatewayError::auth_invalid(
            "WorkBuddy authorization did not identify a user",
        ));
    }
    let realm = region::realm(region);
    let domain = {
        let candidate = text(token.get("domain"));
        if candidate.is_empty() {
            realm.default_domain.to_string()
        } else if realm.accepts_domain(&candidate) {
            candidate
        } else {
            return Err(GatewayError::auth_invalid(
                "WorkBuddy authorization returned a domain from another region",
            ));
        }
    };
    let enterprise_id = {
        let candidate = text(token.get("enterprise_id"));
        if candidate.is_empty() {
            text(account.get("enterpriseId"))
        } else {
            candidate
        }
    };
    let expires_at = token_expiry(token, payload, now);
    Ok(WorkbuddyCredential {
        access_token: access_token.to_string(),
        refresh_token: text(token.get("refreshToken")),
        uid,
        enterprise_id,
        domain,
        region,
        expires_at,
        refresh_expires_at: 0,
    })
}

/// Reference `_expires_at`: an absolute `expires_at` wins, then
/// `created_at + expires_in`, then a duration relative to now.
fn token_expiry(token: &serde_json::Map<String, Value>, payload: &Value, now: i64) -> i64 {
    let absolute = token
        .get("expires_at")
        .and_then(Value::as_i64)
        .or_else(|| payload.get("expires_at").and_then(Value::as_i64));
    if let Some(value) = absolute.filter(|value| *value > 0) {
        return normalize_epoch(value);
    }
    let expires_in = token.get("expires_in").and_then(Value::as_i64).unwrap_or(0);
    if expires_in <= 0 {
        return 0;
    }
    match token.get("created_at").and_then(Value::as_i64) {
        Some(created) if created > 0 => normalize_epoch(created).saturating_add(expires_in),
        _ => now.saturating_add(expires_in),
    }
}

fn normalize_epoch(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn code_of(payload: &Value) -> Option<i64> {
    match payload.get("code")? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

async fn read_body(response: reqwest::Response) -> Result<String, GatewayError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| classify_transport(&error))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(GatewayError::upstream_server(
            "WorkBuddy auth response exceeded the size limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{spawn_server, Reply};

    const STATE: &str = "/v2/plugin/auth/state";
    const TOKEN: &str = "/v2/plugin/auth/token";
    const ACCOUNT: &str = "/v2/plugin/login/account";

    #[tokio::test]
    async fn start_returns_the_official_url_and_state() {
        let server = spawn_server(|request| match request.path() {
            STATE => Reply::json(
                200,
                json!({"code":0,"data":{"authUrl":"https://www.codebuddy.cn/login?x=1","state":"s-1"}})
                    .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let start = client.start(Region::Cn).await.unwrap();
        assert!(start.auth_url.starts_with("https://www.codebuddy.cn/login"));
        assert_eq!(start.upstream_state, "s-1");
        assert_eq!(start.interval_seconds, POLL_INTERVAL_SECONDS);
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert!(request.target.contains("platform=CLI"));
    }

    #[tokio::test]
    async fn pending_codes_keep_the_flow_waiting() {
        let server = spawn_server(|request| match request.path() {
            TOKEN => Reply::json(200, json!({"code": 11217, "msg": "waiting"}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let outcome = client.poll(Region::Cn, "s-1", 2_000, 1_000).await.unwrap();
        assert!(matches!(outcome, WorkbuddyPollOutcome::Pending));
    }

    #[tokio::test]
    async fn an_expired_flow_is_rejected_without_a_request() {
        let server = spawn_server(|_| Reply::json(200, "{}")).await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let error = client
            .poll(Region::Cn, "s-1", 1_000, 1_000)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthExpired);
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn a_completed_flow_builds_a_region_bound_credential() {
        let server = spawn_server(|request| match request.path() {
            TOKEN => Reply::json(
                200,
                json!({"code":0,"data":{
                    "accessToken":"a-1",
                    "refreshToken":"r-1",
                    "domain":"www.codebuddy.cn",
                    "enterprise_id":"ent-1",
                    "expires_in":3600
                }})
                .to_string(),
            ),
            ACCOUNT => Reply::json(
                200,
                json!({"code":0,"data":{"account":{"uid":"uid-1","nickname":"N"}}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let outcome = client.poll(Region::Cn, "s-1", 2_000, 1_000).await.unwrap();
        let WorkbuddyPollOutcome::Completed(credential) = outcome else {
            panic!("expected completion");
        };
        assert_eq!(credential.uid, "uid-1");
        assert_eq!(credential.enterprise_id, "ent-1");
        assert_eq!(credential.domain, "www.codebuddy.cn");
        assert_eq!(credential.expires_at, 4_600);
        assert_eq!(credential.refresh_token, "r-1");
    }

    #[tokio::test]
    async fn an_account_that_is_not_ready_stays_pending() {
        let server = spawn_server(|request| match request.path() {
            TOKEN => Reply::json(
                200,
                json!({"code":0,"data":{"accessToken":"a-1","domain":"www.codebuddy.cn"}})
                    .to_string(),
            ),
            ACCOUNT => Reply::json(200, json!({"code": 12151}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let outcome = client.poll(Region::Cn, "s-1", 2_000, 1_000).await.unwrap();
        assert!(matches!(outcome, WorkbuddyPollOutcome::Pending));
    }

    #[tokio::test]
    async fn a_token_from_another_region_is_refused() {
        let server = spawn_server(|request| match request.path() {
            TOKEN => Reply::json(
                200,
                json!({"code":0,"data":{"accessToken":"a-1","domain":"www.workbuddy.ai"}})
                    .to_string(),
            ),
            ACCOUNT => Reply::json(
                200,
                json!({"code":0,"data":{"account":{"uid":"uid-1"}}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = WorkbuddyAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let error = client
            .poll(Region::Cn, "s-1", 2_000, 1_000)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }
}
