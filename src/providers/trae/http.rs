//! TRAE HTTP client.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §2-§3, built from
//! `Coding2API/src/provider/trae/client.py:256-320` (`solo_headers`) and
//! `dsh-wtq-pool/src/trae-upstream.ts:150-190`.
//!
//! Headers this adapter sends:
//!
//! ```text
//! Authorization: Cloud-IDE-JWT <access token>
//! X-Cloudide-Token / X-Ide-Token: <access token>
//! X-Uid: <uid>                       (when the credential carries one)
//! X-Machine-Id / X-Device-Id: <…>    (only when the operator imported them)
//! Content-Type / Accept
//! User-Agent: polygate/<version>
//! ```
//!
//! The official IDE header family (`X-App-Id`, `X-Ide-Version*`, `X-Device-Type`,
//! `X-Device-Brand`, `X-OS-Version`, `Request-Traffic-Type`, `Trae/…` UA) is
//! absent on purpose: reproducing it would impersonate TRAE's client, which
//! AGENTS.md §2 forbids. The resulting risk (an upstream that insists on these
//! headers) is recorded as a compatibility gap, not hidden.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::Value;

use crate::config::Region;
use crate::error::GatewayError;
use crate::providers::profile::{self, Identity};

use super::credential::TraeCredential;
use super::error::{classify_http_status, classify_transport, is_business_failure};
use super::region;

const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct TraeHttp {
    client: reqwest::Client,
    base: String,
    pay_base: String,
    oauth_base: String,
    region: Region,
    timeout: Duration,
    identity: Identity,
}

impl std::fmt::Debug for TraeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraeHttp")
            .field("base", &self.base)
            .field("pay_base", &self.pay_base)
            .field("oauth_base", &self.oauth_base)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl TraeHttp {
    pub fn new(client: reqwest::Client, region: Region) -> Self {
        let realm = region::realm(region);
        Self {
            client,
            base: realm.chat_base.to_string(),
            pay_base: realm.pay_base.to_string(),
            oauth_base: realm.oauth_base.to_string(),
            region,
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    /// Tests only: point every host at one local mock server.
    pub fn with_base(client: reqwest::Client, region: Region, base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        Self {
            client,
            base: base.clone(),
            pay_base: base.clone(),
            oauth_base: base,
            region,
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = identity;
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// The IDE UA once impersonating, the gateway UA otherwise.
    fn user_agent(&self) -> String {
        match self.identity {
            Identity::Impersonate => profile::trae_ua(),
            Identity::Honest => CLIENT_UA.to_string(),
        }
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

    pub fn chat_url(&self) -> String {
        region::realm(self.region).chat_url(&self.base)
    }

    pub fn models_url(&self) -> String {
        region::realm(self.region).models_url(&self.base)
    }

    pub fn pay_url(&self, path: &str) -> String {
        format!("{}{}", self.pay_base, path)
    }

    /// Short pay/check-in JSON call. `device_id` is the operator-configured
    /// check-in device number; it is never generated here.
    pub async fn post_pay_json(
        &self,
        path: &str,
        body: &Value,
        credential: &TraeCredential,
        device_id: Option<&str>,
    ) -> Result<Value, GatewayError> {
        let url = self.pay_url(path);
        let response = self
            .client
            .post(&url)
            .headers(self.pay_headers(credential, device_id)?)
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = limited_body(response, MAX_JSON_BODY_BYTES).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        if is_business_failure(&body) {
            return Err(classify_http_status(status, &body));
        }
        serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server(format!("TRAE returned unparsable JSON for {url}"))
                .with_upstream_status(status)
        })
    }

    /// Pay/check-in call where the caller must see the status and body even on
    /// failure: the claim reports "already claimed by this device" as a business
    /// code and some builds use HTTP 400.
    pub async fn post_pay_raw(
        &self,
        path: &str,
        body: &Value,
        credential: &TraeCredential,
        device_id: Option<&str>,
    ) -> Result<(u16, String), GatewayError> {
        let url = self.pay_url(path);
        let response = self
            .client
            .post(&url)
            .headers(self.pay_headers(credential, device_id)?)
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = limited_body(response, MAX_ERROR_BODY_BYTES).await?;
        Ok((status, body))
    }

    /// Exchange the refresh token for a new access token. The caller persists
    /// the result with CAS; a rotated refresh token that cannot be stored must
    /// not be used.
    pub async fn refresh(
        &self,
        credential: &TraeCredential,
        now: i64,
    ) -> Result<TraeCredential, GatewayError> {
        if !credential.can_refresh() {
            return Err(GatewayError::new(
                crate::error::ErrorKind::AuthExpired,
                "TRAE credential expired and has no refresh token",
            ));
        }
        let base = if credential.api_host.trim().is_empty() {
            if self.oauth_base.is_empty() {
                return Err(GatewayError::unsupported(
                    "TRAE International token exchange host is not verified",
                ));
            }
            self.oauth_base.clone()
        } else {
            credential.api_host.clone()
        };
        let exchanged =
            super::auth::exchange_refresh_token(&self.client, &base, &credential.refresh_token)
                .await?;
        credential.merge_refreshed(&exchanged, now)
    }

    fn pay_headers(
        &self,
        credential: &TraeCredential,
        device_id: Option<&str>,
    ) -> Result<HeaderMap, GatewayError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Ok(value) = HeaderValue::from_str(&self.user_agent()) {
            headers.insert(USER_AGENT, value);
        }
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Cloud-IDE-JWT {}", credential.access_token)).map_err(
                |_| GatewayError::auth_invalid("TRAE access token cannot be sent in a header"),
            )?,
        );
        headers.insert(
            HeaderName::from_static("x-user-region"),
            HeaderValue::from_static(match self.region {
                Region::Cn => "CN",
                Region::Intl => "AI",
            }),
        );
        for (name, value) in [
            ("X-Uid", credential.uid.as_str()),
            ("X-Machine-Id", credential.machine_id.as_str()),
            ("X-Device-Id", device_id.unwrap_or_default().trim()),
        ] {
            if value.is_empty() {
                continue;
            }
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                GatewayError::auth_invalid("TRAE credential carries an unusable header name")
            })?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                GatewayError::auth_invalid("TRAE credential carries an unusable header value")
            })?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    /// Short JSON call (model directory, and M14-E billing). A business code in
    /// a 2xx envelope is still an error.
    pub async fn post_json(
        &self,
        url: &str,
        body: &Value,
        credential: &TraeCredential,
    ) -> Result<Value, GatewayError> {
        let response = self
            .client
            .post(url)
            .headers(self.headers(credential, "application/json")?)
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let body = limited_body(response, MAX_JSON_BODY_BYTES).await?;
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server(format!("TRAE returned unparsable JSON for {url}"))
                .with_upstream_status(status)
        })?;
        // `llm_utils_chat` errors arrive as an SSE frame; the JSON endpoints
        // report failures in `{code, message}` on HTTP 200.
        if is_business_failure(&body) {
            return Err(classify_http_status(status, &body));
        }
        Ok(payload)
    }

    /// Open the chat stream. A non-2xx response or a JSON body where SSE was
    /// promised is classified here, so the caller only ever sees a stream.
    pub async fn open_stream(
        &self,
        credential: &TraeCredential,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        let url = self.chat_url();
        let response = self
            .client
            .post(&url)
            .headers(self.headers(credential, "text/event-stream")?)
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
            if is_business_failure(&body) {
                return Err(classify_http_status(status, &body));
            }
            return Err(
                GatewayError::upstream_server("TRAE returned a non-SSE chat response")
                    .with_upstream_status(status),
            );
        }
        Ok(response)
    }

    fn headers(
        &self,
        credential: &TraeCredential,
        accept: &'static str,
    ) -> Result<HeaderMap, GatewayError> {
        let token = HeaderValue::from_str(&credential.access_token).map_err(|_| {
            GatewayError::auth_invalid("TRAE access token cannot be sent in a header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        if let Ok(value) = HeaderValue::from_str(&self.user_agent()) {
            headers.insert(USER_AGENT, value);
        }
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Cloud-IDE-JWT {}", credential.access_token)).map_err(
                |_| GatewayError::auth_invalid("TRAE access token cannot be sent in a header"),
            )?,
        );
        headers.insert(HeaderName::from_static("x-cloudide-token"), token.clone());
        headers.insert(HeaderName::from_static("x-ide-token"), token);
        if self.identity.is_impersonating() {
            profile::apply(&mut headers, profile::trae_ide_headers());
        }
        for (name, value) in [
            ("X-Uid", credential.uid.as_str()),
            ("X-Machine-Id", credential.machine_id.as_str()),
            ("X-Device-Id", credential.device_id.as_str()),
        ] {
            if value.trim().is_empty() {
                continue;
            }
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                GatewayError::auth_invalid("TRAE credential carries an unusable header name")
            })?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                GatewayError::auth_invalid("TRAE credential carries an unusable header value")
            })?;
            headers.insert(name, value);
        }
        Ok(headers)
    }
}

async fn limited_body(response: reqwest::Response, limit: usize) -> Result<String, GatewayError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| classify_transport(&error))?;
    if bytes.len() > limit {
        return Err(GatewayError::upstream_server(
            "TRAE response exceeded the size limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
