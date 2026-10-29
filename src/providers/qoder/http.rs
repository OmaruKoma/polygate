//! Qoder OpenAPI HTTP client (plain Bearer, no COSY signature).
//!
//! Source: Coding2API `src/provider/qoder/client.py:103` (`build_openapi_headers`)
//! and `:599` (`_get_json`). Only endpoints that the reference calls without a
//! COSY signature live here: `userinfo`, `quota/usage` and the campaign /
//! daily-check-in family. Inference and the model catalogue are signed requests
//! and live in [`super::gateway`].
//!
//! `X-Machine-ID` / `X-Session-ID` are derived from the uid instead of being
//! random, because the reference documents that the same account must present a
//! consistent device identity across the OpenAPI and COSY sides.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::Value;

use crate::error::GatewayError;
use crate::providers::profile::{self, Identity};

use super::cosy::{self, CLIENT_UA};
use super::credential::QoderCredential;
use super::error::{classify_http_status, classify_payload, classify_transport};
use super::region;

const MAX_BODY_BYTES: usize = 128 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// One realm's OpenAPI base URL. The override exists for deterministic tests and
/// is never read from production configuration.
#[derive(Clone)]
pub struct QoderApi {
    client: reqwest::Client,
    base: String,
    website: String,
    timeout: Duration,
    identity: Identity,
}

impl std::fmt::Debug for QoderApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderApi")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl QoderApi {
    pub fn new(client: reqwest::Client, region: crate::config::Region) -> Self {
        let realm = region::realm(region);
        Self {
            client,
            base: realm.openapi_base.trim_end_matches('/').to_string(),
            website: realm.website.to_string(),
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    /// In-place variant, for callers that own the value through an `Arc`.
    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = identity;
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// Tests only: point one realm at a local mock server.
    pub fn with_base(
        client: reqwest::Client,
        region: crate::config::Region,
        base: impl Into<String>,
    ) -> Self {
        let realm = region::realm(region);
        Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
            website: realm.website.to_string(),
            timeout: REQUEST_TIMEOUT,
            identity: Identity::default(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn get(
        &self,
        path: &str,
        credential: &QoderCredential,
    ) -> Result<Value, GatewayError> {
        self.get_with_headers(path, credential, &[]).await
    }

    pub async fn get_with_headers(
        &self,
        path: &str,
        credential: &QoderCredential,
        extra: &[(&str, &str)],
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.base, path);
        let request = self
            .client
            .get(&url)
            .headers(self.headers(credential, extra))
            .timeout(self.timeout);
        self.send(request, &url).await
    }

    pub async fn post(
        &self,
        path: &str,
        body: &Value,
        credential: &QoderCredential,
    ) -> Result<Value, GatewayError> {
        self.post_with_headers(path, body, credential, &[]).await
    }

    pub async fn post_with_headers(
        &self,
        path: &str,
        body: &Value,
        credential: &QoderCredential,
        extra: &[(&str, &str)],
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.base, path);
        let request = self
            .client
            .post(&url)
            .headers(self.headers(credential, extra))
            .json(body)
            .timeout(self.timeout);
        self.send(request, &url).await
    }

    fn headers(&self, credential: &QoderCredential, extra: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/plain, */*"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_UA));
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", credential.access_token)) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        let machine = cosy::derive_id(&credential.uid, "machine");
        let session = cosy::derive_id(&credential.uid, "session");
        for (name, value) in [
            ("x-request-id", cosy::new_request_id()),
            ("x-machine-id", machine),
            ("x-session-id", session),
            ("origin", self.website.clone()),
            ("referer", format!("{}/", self.website)),
        ] {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        }
        if self.identity.is_impersonating() {
            // The campaign family depends on `cosy-clienttype`; the rest of the
            // desktop identity keeps the OpenAPI side consistent with the COSY
            // device derived from the same uid.
            for (name, value) in [
                ("cosy-clienttype", profile::QODER_CLIENT_TYPE.to_string()),
                (
                    "cosy-machinetype",
                    profile::qoder_machine_type(&credential.uid),
                ),
                (
                    "cosy-machineid",
                    cosy::derive_id(&credential.uid, "machine"),
                ),
                (
                    "cosy-machinetoken",
                    profile::qoder_machine_token(&credential.uid),
                ),
                ("cosy-clientip", profile::QODER_CLIENT_IP.to_string()),
                ("cosy-version", cosy::COSY_VERSION.to_string()),
                ("cosy-data-policy", "AGREE".to_string()),
                ("login-version", profile::QODER_LOGIN_VERSION.to_string()),
            ] {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    headers.insert(name, value);
                }
            }
        }
        for (name, value) in extra {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        headers
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<Value, GatewayError> {
        let response = request
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| classify_transport(&error))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(GatewayError::upstream_server(
                "Qoder response exceeded the size limit",
            ));
        }
        let body = String::from_utf8_lossy(&bytes);
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server(format!("Qoder returned unparsable JSON for {url}"))
                .with_upstream_status(status)
        })?;
        if let Some(error) = classify_payload(&payload) {
            return Err(error);
        }
        Ok(payload)
    }
}
