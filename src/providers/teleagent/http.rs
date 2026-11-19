//! TeleAgent HTTP client.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md` §4–§6, §9.
//!
//! * Every call carries the product constant (`Authorization: Bearer sk-…`)
//!   plus the account token (`X-Token: <tenant>:<JWT>`).
//! * `POST /chat/completions` additionally carries the `X-SA-*` signature set.
//! * Read-only GETs only need `X-Token` plus a nonce/timestamp pair; that is
//!   what the web build sends and what the upstream accepts (measured).
//! * The User-Agent stays `polygate/<version>`: the gateway does not impersonate
//!   the desktop client, and the upstream does not check the UA.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{ErrorKind, GatewayError};

use super::credential::TeleagentCredential;
use super::region;
use super::signer;

const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct TeleagentHttp {
    client: reqwest::Client,
    base: String,
    timeout: Duration,
}

impl std::fmt::Debug for TeleagentHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeleagentHttp")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl TeleagentHttp {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base: region::BASE.to_string(),
            timeout: Duration::from_secs(region::REQUEST_TIMEOUT_SECONDS),
        }
    }

    /// Tests only: point the adapter at one local mock server.
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

    fn headers(&self, credential: &TeleagentCredential, accept: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {}", region::CLIENT_API_KEY);
        if let Ok(value) = HeaderValue::from_str(&bearer) {
            headers.insert(AUTHORIZATION, value);
        }
        if let Ok(value) = HeaderValue::from_str(&credential.token) {
            headers.insert(HeaderName::from_static("x-token"), value);
        }
        if let Ok(value) = HeaderValue::from_str(&Uuid::new_v4().to_string()) {
            headers.insert(HeaderName::from_static("x-nonce"), value);
        }
        let now = chrono::Utc::now().timestamp().to_string();
        if let Ok(value) = HeaderValue::from_str(&now) {
            headers.insert(HeaderName::from_static("x-timestamp"), value);
        }
        if let Ok(value) = HeaderValue::from_str(&credential.app_version) {
            headers.insert(HeaderName::from_static("x-app-version"), value);
        }
        headers.insert(
            HeaderName::from_static("x-route-target"),
            HeaderValue::from_static(region::ROUTE_TARGET),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_str(accept).unwrap_or_else(|_| HeaderValue::from_static("*/*")),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_UA));
        headers
    }

    /// Add the signed-header set. `uri` is path + query, never the host.
    fn signed_headers(
        &self,
        credential: &TeleagentCredential,
        accept: &str,
        method: &str,
        uri: &str,
        body: &[u8],
    ) -> HeaderMap {
        let mut headers = self.headers(credential, accept);
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let nonce = Uuid::new_v4().to_string();
        let step = signer::step_key(
            &credential.token,
            &credential.install_id,
            &timestamp,
            &nonce,
        );
        let signature = signer::sign(
            &step,
            method,
            uri,
            &timestamp,
            &nonce,
            &credential.app_version,
            body,
        );
        let fields = [
            ("x-superagent-timestamp", timestamp.as_str()),
            ("x-superagent-nonce", nonce.as_str()),
            ("x-superagent-signature", signature.as_str()),
            ("x-superagent-sign-version", region::SIGN_VERSION),
            ("x-superagent-install-id", credential.install_id.as_str()),
            ("x-superagent-device-id", credential.device_id.as_str()),
        ];
        for (name, value) in fields {
            if let Ok(value) = HeaderValue::from_str(value) {
                headers.insert(HeaderName::from_static(name), value);
            }
        }
        headers
    }

    pub async fn get_model_options(
        &self,
        credential: &TeleagentCredential,
    ) -> Result<Value, GatewayError> {
        let path = format!("{}?language=zh-CN", region::MODEL_OPTIONS_PATH);
        let request = self
            .client
            .get(format!("{}{}", self.base, path))
            .version(reqwest::Version::HTTP_11)
            .headers(self.headers(credential, "application/json"))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        self.send_json(request).await
    }

    pub async fn get_user_center(
        &self,
        credential: &TeleagentCredential,
    ) -> Result<Value, GatewayError> {
        let request = self
            .client
            .get(format!("{}{}", self.base, region::USER_CENTER_PATH))
            .version(reqwest::Version::HTTP_11)
            .headers(self.headers(credential, "application/json"))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        self.send_json(request).await
    }

    /// Company-store portal GET. `path_and_query` must be one of the
    /// `region::PORTAL_*` constants — never caller-supplied text.
    pub async fn get_portal_json(
        &self,
        credential: &TeleagentCredential,
        path_and_query: &str,
    ) -> Result<Value, GatewayError> {
        let request = self
            .client
            .get(format!("{}{}", self.base, path_and_query))
            .version(reqwest::Version::HTTP_11)
            .headers(self.headers(credential, "application/json"))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        self.send_json(request).await
    }

    /// Signed JSON chat call. `body` is the exact byte payload that was signed.
    pub async fn post_chat_json(
        &self,
        credential: &TeleagentCredential,
        body: &[u8],
    ) -> Result<Value, GatewayError> {
        let request = self
            .client
            .post(format!("{}{}", self.base, region::CHAT_PATH))
            .version(reqwest::Version::HTTP_11)
            .headers(self.signed_headers(
                credential,
                "application/json",
                "POST",
                region::CHAT_PATH,
                body,
            ))
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .timeout(self.timeout);
        self.send_json(request).await
    }

    /// Open the SSE stream with the same signature set as the JSON call.
    pub async fn open_stream(
        &self,
        credential: &TeleagentCredential,
        body: &[u8],
    ) -> Result<reqwest::Response, GatewayError> {
        let response = self
            .client
            .post(format!("{}{}", self.base, region::CHAT_PATH))
            .version(reqwest::Version::HTTP_11)
            .headers(self.signed_headers(
                credential,
                "text/event-stream",
                "POST",
                region::CHAT_PATH,
                body,
            ))
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_vec())
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
        let value = limited_json(response).await?;
        if let Some(error) = envelope_error(&value) {
            return Err(error);
        }
        Ok(value)
    }
}

async fn limited_json(response: reqwest::Response) -> Result<Value, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    if bytes.len() > region::max_json_body() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "TeleAgent returned an oversized JSON body",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        GatewayError::new(
            ErrorKind::UpstreamServer,
            "TeleAgent returned a body that is not JSON",
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
        GatewayError::new(ErrorKind::Timeout, "TeleAgent request timed out")
    } else {
        GatewayError::new(ErrorKind::Transport, "TeleAgent request failed")
    }
}

/// Upstream error codes (migration note §12).
fn code_error(code: i64, detail: &str) -> GatewayError {
    match code {
        4001 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("TeleAgent token is missing or was rejected (4001){detail}"),
        ),
        40108 => GatewayError::new(
            ErrorKind::AuthExpired,
            format!("TeleAgent token is invalid; import a fresh one (40108){detail}"),
        ),
        40101 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("TeleAgent rejected the security headers (40101){detail}"),
        ),
        40107 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("TeleAgent lost the user identity (40107){detail}"),
        ),
        _ => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("TeleAgent returned code {code}{detail}"),
        ),
    }
}

fn classify_status(status: u16, body: &str) -> GatewayError {
    let detail = summarize(body);
    if let Some(code) = body_code(body) {
        let mut error = code_error(code, &detail);
        if status == 429 {
            error = GatewayError::new(ErrorKind::RateLimited, error.message);
        }
        return error;
    }
    match status {
        401 => GatewayError::new(
            ErrorKind::AuthExpired,
            format!("TeleAgent rejected the login token; import a fresh one{detail}"),
        ),
        403 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("TeleAgent refused this account{detail}"),
        ),
        402 => GatewayError::new(
            ErrorKind::QuotaExhausted,
            format!("TeleAgent reports exhausted points{detail}"),
        ),
        404 => GatewayError::new(
            ErrorKind::ModelUnavailable,
            format!("TeleAgent resource was not found{detail}"),
        ),
        429 => GatewayError::new(
            ErrorKind::RateLimited,
            format!("TeleAgent rate limited{detail}"),
        ),
        status if status >= 500 => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("TeleAgent upstream error {status}{detail}"),
        ),
        status => GatewayError::new(
            ErrorKind::BadRequest,
            format!("TeleAgent rejected the request ({status}){detail}"),
        ),
    }
}

/// A `200` carrying an error envelope instead of a payload.
fn classify_payload(value: &Value) -> GatewayError {
    envelope_error(value).unwrap_or_else(|| {
        GatewayError::new(
            ErrorKind::UpstreamServer,
            "TeleAgent answered with an error envelope",
        )
    })
}

/// `{"code": 4001, "message": …}` is an error; `code` 0/200 is success.
fn envelope_error(value: &Value) -> Option<GatewayError> {
    let code = value.get("code")?.as_i64()?;
    if code == 0 || code == 200 {
        return None;
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("msg").and_then(Value::as_str))
        .unwrap_or_default();
    Some(code_error(code, &summarize(message)))
}

fn body_code(body: &str) -> Option<i64> {
    let value: Value = serde_json::from_str(body).ok()?;
    match value.get("code")? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn summarize(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    let slice: String = body.chars().take(200).collect();
    format!(": {slice}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_auth_code_becomes_an_auth_error() {
        let error = classify_status(200, r#"{"code":40108,"message":"invalid token"}"#);
        assert_eq!(error.kind, ErrorKind::AuthExpired);
        let error = classify_status(401, r#"{"code":"4001"}"#);
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[test]
    fn a_plain_401_becomes_an_expired_auth_error() {
        assert_eq!(classify_status(401, "").kind, ErrorKind::AuthExpired);
        assert_eq!(classify_status(429, "").kind, ErrorKind::RateLimited);
        assert_eq!(classify_status(503, "").kind, ErrorKind::UpstreamServer);
    }

    #[test]
    fn a_success_envelope_is_not_an_error() {
        assert!(envelope_error(&serde_json::json!({"code":0,"data":{}})).is_none());
        assert!(envelope_error(&serde_json::json!({"code":200,"data":{}})).is_none());
        assert!(envelope_error(&serde_json::json!({"choices":[]})).is_none());
    }
}
