//! Loomy model-gateway HTTP client.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md` §3. Every model call carries the login
//! session twice (`Authorization: Bearer` and `token`), a W3C `traceparent`
//! (the gateway **hangs until timeout** without it) and a presence-checked
//! `loomy-version`. The User-Agent stays the gateway's own
//! `polygate/<version>`: the upstream only checks `loomy-version`.
//!
//! The account service (`account.xfinfr.com`) is only needed to obtain a
//! session, which happens in the operator's browser/client today; this client
//! therefore talks to the model gateway alone.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{ErrorKind, GatewayError};

use super::credential::LoomyCredential;
use super::region;

const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct LoomyHttp {
    client: reqwest::Client,
    base: String,
    timeout: Duration,
}

impl std::fmt::Debug for LoomyHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoomyHttp")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl LoomyHttp {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base: region::MODEL_BASE.to_string(),
            timeout: Duration::from_secs(region::REQUEST_TIMEOUT_SECONDS),
        }
    }

    /// Tests only: point the model gateway at one local mock server.
    pub fn with_base(client: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(region::REQUEST_TIMEOUT_SECONDS),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn headers(&self, credential: &LoomyCredential, accept: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", credential.session);
        if let Ok(value) = HeaderValue::from_str(&bearer) {
            headers.insert(AUTHORIZATION, value);
        }
        if let Ok(value) = HeaderValue::from_str(&credential.session) {
            headers.insert(HeaderName::from_static("token"), value);
        }
        if let Ok(value) = HeaderValue::from_str(&traceparent()) {
            headers.insert(HeaderName::from_static("traceparent"), value);
        }
        headers.insert(
            HeaderName::from_static("loomy-version"),
            HeaderValue::from_static(region::LOOMY_VERSION),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_str(accept).unwrap_or_else(|_| HeaderValue::from_static("*/*")),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_UA));
        if accept == "application/json" {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        headers
    }

    pub async fn get_models(&self, credential: &LoomyCredential) -> Result<Value, GatewayError> {
        let url = self.url(region::MODELS_PATH);
        let request = self
            .client
            .get(&url)
            .headers(self.headers(credential, "application/json"))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        self.send_json(request).await
    }

    pub async fn get_points(
        &self,
        credential: &LoomyCredential,
        page_size: u32,
    ) -> Result<Value, GatewayError> {
        let url = format!(
            "{}?record_type=all&page_no=1&page_size={page_size}",
            self.url(region::POINTS_PATH)
        );
        let request = self
            .client
            .get(&url)
            .headers(self.headers(credential, "application/json"))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        self.send_json(request).await
    }

    pub async fn post_chat_json(
        &self,
        credential: &LoomyCredential,
        body: &Value,
    ) -> Result<Value, GatewayError> {
        let url = self.url(region::CHAT_PATH);
        let request = self
            .client
            .post(&url)
            .headers(self.headers(credential, "application/json"))
            .json(body)
            .timeout(self.timeout);
        self.send_json(request).await
    }

    /// Open the SSE stream. A JSON error envelope returned with HTTP 200 is
    /// classified here so the caller only ever sees a stream.
    pub async fn open_stream(
        &self,
        credential: &LoomyCredential,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        let url = self.url(region::CHAT_PATH);
        let response = self
            .client
            .post(&url)
            .headers(self.headers(credential, "text/event-stream"))
            .json(body)
            .send()
            .await
            .map_err(classify_transport)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if status != 200 {
            let body = limited_body(response).await?;
            return Err(classify_status(status, &body));
        }
        if content_type.contains("application/json") {
            let value = limited_json(response).await?;
            return Err(classify_payload(&value));
        }
        Ok(response)
    }

    async fn send_json(&self, request: reqwest::RequestBuilder) -> Result<Value, GatewayError> {
        let response = request.send().await.map_err(classify_transport)?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = limited_body(response).await?;
            return Err(classify_status(status, &body));
        }
        limited_json(response).await
    }
}

async fn limited_json(response: reqwest::Response) -> Result<Value, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    if bytes.len() > region::max_json_body() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "Loomy returned an oversized JSON body",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        GatewayError::new(
            ErrorKind::UpstreamServer,
            "Loomy returned a body that is not JSON",
        )
    })
}

async fn limited_body(response: reqwest::Response) -> Result<String, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    let slice = &bytes[..bytes.len().min(MAX_ERROR_BODY_BYTES)];
    Ok(String::from_utf8_lossy(slice).to_string())
}

pub fn classify_transport(error: reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::new(ErrorKind::Timeout, "Loomy request timed out")
    } else {
        GatewayError::new(ErrorKind::Transport, "Loomy request failed")
    }
}

fn classify_status(status: u16, body: &str) -> GatewayError {
    let detail = summarize(body);
    match status {
        401 => GatewayError::new(
            ErrorKind::AuthExpired,
            format!("Loomy session was rejected; import a fresh session{detail}"),
        ),
        403 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("Loomy refused this account{detail}"),
        ),
        402 => GatewayError::new(
            ErrorKind::QuotaExhausted,
            format!("Loomy reports exhausted points{detail}"),
        ),
        404 => GatewayError::new(
            ErrorKind::ModelUnavailable,
            format!("Loomy resource was not found{detail}"),
        ),
        429 => GatewayError::new(
            ErrorKind::RateLimited,
            format!("Loomy rate limited{detail}"),
        ),
        status if status >= 500 => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("Loomy upstream error {status}{detail}"),
        ),
        status => GatewayError::new(
            ErrorKind::BadRequest,
            format!("Loomy rejected the request ({status}){detail}"),
        ),
    }
}

/// A `200` with a JSON error envelope (Loomy does this for some failures).
fn classify_payload(value: &Value) -> GatewayError {
    let code = value.get("code").and_then(Value::as_i64).or_else(|| {
        value
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64)
    });
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("desc").and_then(Value::as_str))
        .or_else(|| {
            value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unexpected body");
    match code {
        Some(401) => GatewayError::auth_invalid("Loomy session was rejected"),
        Some(402) => GatewayError::new(ErrorKind::QuotaExhausted, "Loomy reports exhausted points"),
        Some(429) => GatewayError::new(ErrorKind::RateLimited, "Loomy rate limited"),
        _ => GatewayError::new(ErrorKind::UpstreamServer, format!("Loomy error: {message}")),
    }
}

fn summarize(body: &str) -> String {
    let text = body.trim();
    if text.is_empty() {
        return String::new();
    }
    let clipped: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
    format!(": {clipped}")
}

/// W3C trace context: version 00, random trace id, random span id.
pub fn traceparent() -> String {
    let trace = Uuid::new_v4().simple().to_string();
    let span = &Uuid::new_v4().simple().to_string()[..16];
    format!("00-{trace}-{span}-01")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traceparent_has_the_w3c_shape() {
        let value = traceparent();
        let parts: Vec<&str> = value.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "00");
        assert_eq!(parts[1].len(), 32);
        assert_eq!(parts[2].len(), 16);
        assert_eq!(parts[3], "01");
        assert!(parts[1].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn statuses_map_onto_the_gateway_taxonomy() {
        assert_eq!(classify_status(401, "").kind, ErrorKind::AuthExpired);
        assert_eq!(classify_status(403, "").kind, ErrorKind::AuthInvalid);
        assert_eq!(classify_status(402, "").kind, ErrorKind::QuotaExhausted);
        assert_eq!(classify_status(429, "").kind, ErrorKind::RateLimited);
        assert_eq!(classify_status(503, "").kind, ErrorKind::UpstreamServer);
        assert_eq!(classify_status(400, "").kind, ErrorKind::BadRequest);
        assert_eq!(classify_status(404, "").kind, ErrorKind::ModelUnavailable);
    }

    #[test]
    fn a_200_error_envelope_is_not_treated_as_success() {
        let error = classify_payload(&serde_json::json!({
            "code": 401,
            "message": "token invalid"
        }));
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }
}
