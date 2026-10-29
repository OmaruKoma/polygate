//! WorkBuddy HTTP client.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §2 and §3.2.
//!
//! Two header sets exist. The honest one sends only what the protocol needs
//! (`Authorization`, account ids, domain, `Content-Type`/`Accept`, the gateway's
//! own UA). With [`Identity::Impersonate`] the desktop client's header family is
//! sent as well: gate headers, conversation ids, `x-stainless-*`, IDE/agent
//! constants and the official UA. The Intl gateway answers business code 11128
//! to a request with no conversation id, so impersonation is the working mode
//! there and the honest mode is kept for comparison.
//!
//! Request classes matter: chat carries the conversation family, the refresh
//! endpoint carries `X-Refresh-Token` and nothing else does, billing uses the
//! desktop UA. A single flat header map would put chat ids on quota calls.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::Value;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::profile::{self, Identity};

use super::credential::WorkbuddyCredential;
use super::error::{classify_http_status, classify_payload, classify_transport};
use super::region;

const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));

/// Which upstream call the headers are for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    /// Chat stream: conversation id family and the chat UA.
    Chat,
    /// Catalogue / quota JSON.
    Json,
    /// Billing `/meter/*` endpoints.
    Billing,
    /// Token refresh.
    Refresh,
}

#[derive(Clone)]
pub struct WorkbuddyHttp {
    client: reqwest::Client,
    base: String,
    billing_base: String,
    region: Region,
    timeout: Duration,
    identity: Identity,
}

impl std::fmt::Debug for WorkbuddyHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkbuddyHttp")
            .field("base", &self.base)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl WorkbuddyHttp {
    pub fn new(client: reqwest::Client, region: Region) -> Self {
        let realm = region::realm(region);
        Self {
            client,
            base: realm.chat_base.to_string(),
            billing_base: realm.billing_base.to_string(),
            region,
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    /// Tests only: point one region at a local mock server.
    pub fn with_base(client: reqwest::Client, region: Region, base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        Self {
            client,
            base: base.clone(),
            billing_base: base,
            region,
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    /// Builder form; see [`WorkbuddyHttp::set_identity`] for the in-place one.
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.set_identity(identity);
        self
    }

    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = identity;
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    #[cfg(test)]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[cfg(test)]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub async fn get_json(
        &self,
        path: &str,
        credential: &WorkbuddyCredential,
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.base, path);
        let request = self
            .client
            .get(&url)
            .headers(self.headers(credential, "application/json", Call::Json))
            .timeout(self.timeout);
        self.send_json(request, &url).await
    }

    /// Billing endpoints live on a different host than chat. The path is the
    /// same on both realms; only the base changes.
    pub async fn post_billing_json(
        &self,
        path: &str,
        body: &Value,
        credential: &WorkbuddyCredential,
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.billing_base, path);
        let request = self
            .client
            .post(&url)
            .headers(self.headers(credential, "application/json", Call::Billing))
            .json(body)
            .timeout(self.timeout);
        self.send_json(request, &url).await
    }

    /// Billing call where the caller must see the status and body even on a
    /// failure. The daily check-in reports "already claimed" as HTTP 400 with a
    /// business code, so the generic `send_json` would erase the distinction.
    pub async fn post_billing_raw(
        &self,
        path: &str,
        body: &Value,
        credential: &WorkbuddyCredential,
    ) -> Result<(u16, String), GatewayError> {
        let url = format!("{}{}", self.billing_base, path);
        let response = self
            .client
            .post(&url)
            .headers(self.headers(credential, "application/json", Call::Billing))
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = limited_body(response, MAX_ERROR_BODY_BYTES).await?;
        Ok((status, body))
    }

    /// Open the chat stream. A JSON error envelope returned with HTTP 200 or a
    /// non-SSE body is classified here, so the caller only ever sees a stream.
    pub async fn open_stream(
        &self,
        credential: &WorkbuddyCredential,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        let url = region::realm(self.region).chat_url(&self.base);
        let response = self
            .client
            .post(&url)
            .headers(self.headers(credential, "text/event-stream", Call::Chat))
            .json(body)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if !(200..300).contains(&status) {
            let body = limited_body(response, MAX_ERROR_BODY_BYTES).await?;
            return Err(classify_http_status(status, &body));
        }
        if !content_type.contains("event-stream") {
            let body = limited_body(response, MAX_ERROR_BODY_BYTES).await?;
            if let Ok(payload) = serde_json::from_str::<Value>(&body) {
                if let Some(error) = classify_payload(&payload) {
                    return Err(error.with_upstream_status(status));
                }
            }
            return Err(GatewayError::upstream_server(
                "WorkBuddy returned a non-SSE chat response",
            )
            .with_upstream_status(status));
        }
        Ok(response)
    }

    /// Exchange the refresh token for a new access token. The caller persists
    /// the result with CAS; a rotated refresh token that cannot be stored must
    /// not be used.
    pub async fn refresh(
        &self,
        credential: &WorkbuddyCredential,
        now: i64,
    ) -> Result<WorkbuddyCredential, GatewayError> {
        if credential.refresh_token.is_empty() {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "WorkBuddy credential expired and has no refresh token",
            ));
        }
        if !credential.can_refresh(now) {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "WorkBuddy refresh token expired; authorize the account again",
            ));
        }
        let url = region::realm(self.region).url_at(&self.base, region::REFRESH_PATH);
        let refresh_token = HeaderValue::from_str(&credential.refresh_token)
            .map_err(|_| GatewayError::auth_invalid("WorkBuddy refresh token cannot be sent"))?;
        let mut headers = self.headers(
            credential,
            "application/json, text/plain, */*",
            Call::Refresh,
        );
        headers.insert(HeaderName::from_static("x-refresh-token"), refresh_token);
        // The official plugin marks the refresh source; the reference sends it.
        headers.insert(
            HeaderName::from_static("x-auth-refresh-source"),
            HeaderValue::from_static("plugin"),
        );
        let request = self
            .client
            .post(&url)
            .headers(headers)
            .json(&serde_json::json!({}))
            .timeout(self.timeout);
        let response = request
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            // A rejected refresh token is terminal: retrying the same value
            // cannot succeed, the account must be authorized again.
            return Err(GatewayError::auth_invalid(
                "WorkBuddy refresh token was rejected; authorize the account again",
            )
            .with_upstream_status(status));
        }
        let body = limited_body(response, MAX_ERROR_BODY_BYTES).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy refresh returned unparsable JSON")
        })?;
        if let Some(error) = classify_payload(&payload) {
            return Err(error.with_upstream_status(status));
        }
        let data = payload
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GatewayError::upstream_server("WorkBuddy refresh response has no data object")
            })?;
        credential.merge_refreshed(&Value::Object(data.clone()), now)
    }

    fn headers(
        &self,
        credential: &WorkbuddyCredential,
        accept: &'static str,
        call: Call,
    ) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        let ua = match (self.identity, call) {
            (Identity::Impersonate, Call::Chat) => profile::workbuddy_chat_ua(self.region),
            (Identity::Impersonate, _) => profile::workbuddy_desktop_ua(self.region),
            (Identity::Honest, _) => CLIENT_UA.to_string(),
        };
        if let Ok(value) = HeaderValue::from_str(&ua) {
            headers.insert(USER_AGENT, value);
        }
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", credential.access_token)) {
            headers.insert(AUTHORIZATION, value);
        }
        for (name, value) in [
            ("X-User-Id", credential.uid.as_str()),
            ("X-Enterprise-Id", credential.enterprise_id.as_str()),
            ("X-Tenant-Id", credential.enterprise_id.as_str()),
            ("X-Domain", credential.domain.as_str()),
        ] {
            if value.trim().is_empty() {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        if self.identity == Identity::Impersonate {
            match call {
                Call::Chat => profile::apply(
                    &mut headers,
                    profile::workbuddy_chat_headers(
                        self.region,
                        &self.billing_base,
                        &credential.uid,
                        &credential.enterprise_id,
                        &credential.domain,
                    ),
                ),
                Call::Refresh => profile::apply(&mut headers, profile::workbuddy_gate_headers()),
                Call::Billing | Call::Json => {
                    profile::apply(&mut headers, profile::workbuddy_gate_headers());
                    profile::apply(
                        &mut headers,
                        profile::workbuddy_origin_headers(&self.billing_base),
                    );
                }
            }
        }
        headers
    }

    async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<Value, GatewayError> {
        let response = request
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = limited_body(response, MAX_JSON_BODY_BYTES).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server(format!("WorkBuddy returned unparsable JSON for {url}"))
                .with_upstream_status(status)
        })?;
        if let Some(error) = classify_payload(&payload) {
            return Err(error.with_upstream_status(status));
        }
        Ok(payload)
    }
}

async fn limited_body(response: reqwest::Response, limit: usize) -> Result<String, GatewayError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| classify_transport(&error))?;
    if bytes.len() > limit {
        return Err(GatewayError::upstream_server(
            "WorkBuddy response exceeded the size limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
