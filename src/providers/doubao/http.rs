//! Doubao `/chat/completion` HTTP client.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md` §3. Every call carries the operator's
//! own cookies, the CSRF header taken from them, and the device identity that
//! belongs to that login (`device_id`, `web_id`, `fp`).
//!
//! Measured on 2026-11-13: `Origin`, `Referer` and a browser User-Agent are
//! **not** required — the same request without them succeeds. They are
//! therefore not impersonated. `device_id`/`web_id`/`fp` **are** required: a
//! request carrying only the session cookies is answered with
//! `710022004 rate limited` plus a slide-verification decision, so they are
//! imported rather than generated.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, USER_AGENT,
};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{ErrorKind, GatewayError};

use super::credential::DoubaoCredential;
use super::region;

const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct DoubaoHttp {
    client: reqwest::Client,
    base: String,
}

impl std::fmt::Debug for DoubaoHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubaoHttp")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl DoubaoHttp {
    pub fn new(client: reqwest::Client) -> Self {
        Self::with_base(client, region::CHAT_BASE)
    }

    /// Tests only: point the client at one local mock server.
    pub fn with_base(client: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Public parameters the upstream expects on every turn.
    fn query(&self, credential: &DoubaoCredential) -> Vec<(String, String)> {
        let mut pairs = vec![
            ("aid".to_string(), region::APP_ID.to_string()),
            ("real_aid".to_string(), region::APP_ID.to_string()),
            ("device_id".to_string(), credential.device_id.clone()),
            ("tea_uuid".to_string(), credential.device_id.clone()),
            ("web_id".to_string(), credential.web_id.clone()),
            ("device_platform".to_string(), "web".to_string()),
            ("language".to_string(), "zh".to_string()),
            ("region".to_string(), "CN".to_string()),
            ("sys_region".to_string(), "CN".to_string()),
            ("pkg_type".to_string(), "release_version".to_string()),
            ("version_code".to_string(), region::VERSION_CODE.to_string()),
            ("pc_version".to_string(), region::PC_VERSION.to_string()),
            (
                "chromium_version".to_string(),
                region::CHROMIUM_VERSION.to_string(),
            ),
            (
                "client_platform".to_string(),
                region::CLIENT_PLATFORM.to_string(),
            ),
            ("runtime".to_string(), "web".to_string()),
            (
                "runtime_version".to_string(),
                region::RUNTIME_VERSION.to_string(),
            ),
            ("samantha_web".to_string(), "1".to_string()),
            ("use-olympus-account".to_string(), "1".to_string()),
            ("fp".to_string(), credential.fp.clone()),
            ("web_tab_id".to_string(), Uuid::new_v4().to_string()),
        ];
        if !credential.ms_token.is_empty() {
            pairs.push(("msToken".to_string(), credential.ms_token.clone()));
        }
        pairs
    }

    fn headers(&self, credential: &DoubaoCredential, accept: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_static(CLIENT_UA));
        if let Ok(value) = HeaderValue::from_str(&credential.cookie_header()) {
            headers.insert(COOKIE, value);
        }
        if !credential.csrf_token.is_empty() {
            if let Ok(value) = HeaderValue::from_str(&credential.csrf_token) {
                headers.insert(HeaderName::from_static("x-tt-passport-csrf-token"), value);
            }
        }
        headers
    }

    /// Open the SSE stream. The upstream answers a JSON error envelope for
    /// session failures; it is classified here so the caller only sees a stream.
    pub async fn open_stream(
        &self,
        credential: &DoubaoCredential,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        if let Some(missing) = credential.missing_identity() {
            return Err(GatewayError::new(
                ErrorKind::AuthInvalid,
                format!(
                    "Doubao credential is missing `{missing}`; import the desktop login \
                     instead of generating one (the upstream treats a fresh device \
                     identity as a risk signal)"
                ),
            ));
        }
        let url = format!("{}{}", self.base, region::CHAT_PATH);
        let request = self
            .client
            .post(&url)
            .query(&self.query(credential))
            .headers(self.headers(credential, "text/event-stream"))
            .json(body)
            .timeout(Duration::from_secs(region::REQUEST_TIMEOUT_SECONDS));
        let response = request.send().await.map_err(classify_transport)?;
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

    /// Subscription/quota summary. Unlike the other `/alice/*` endpoints this
    /// one answers without the `a_bogus` signature (measured 2026-11-13).
    pub async fn get_quota_summary(
        &self,
        credential: &DoubaoCredential,
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.base, region::QUOTA_PATH);
        let request = self
            .client
            .post(&url)
            .query(&self.query(credential))
            .headers(self.headers(credential, "application/json"))
            .json(&serde_json::json!({ "product_line": region::QUOTA_PRODUCT_LINE }))
            .timeout(Duration::from_secs(region::JSON_TIMEOUT_SECONDS));
        let response = request.send().await.map_err(classify_transport)?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = limited_body(response).await?;
            return Err(classify_status(status, &body));
        }
        let value = limited_json(response).await?;
        match value.get("code").and_then(Value::as_i64) {
            Some(0) | None => Ok(value),
            Some(code) => Err(GatewayError::new(
                ErrorKind::UpstreamServer,
                format!("Doubao quota summary rejected the request (code {code})"),
            )),
        }
    }
}

async fn limited_json(response: reqwest::Response) -> Result<Value, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    if bytes.len() > region::max_json_body() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "Doubao returned an oversized JSON body",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        GatewayError::new(
            ErrorKind::UpstreamServer,
            "Doubao returned a body that is not JSON",
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
        GatewayError::new(ErrorKind::Timeout, "Doubao request timed out")
    } else {
        GatewayError::new(ErrorKind::Transport, "Doubao request failed")
    }
}

fn classify_status(status: u16, body: &str) -> GatewayError {
    let detail = summarize(body);
    match status {
        401 => GatewayError::new(
            ErrorKind::AuthExpired,
            format!("Doubao session was rejected; import a fresh login{detail}"),
        ),
        403 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("Doubao refused this account{detail}"),
        ),
        402 => GatewayError::new(
            ErrorKind::QuotaExhausted,
            format!("Doubao reports no remaining quota{detail}"),
        ),
        404 => GatewayError::new(
            ErrorKind::ModelUnavailable,
            format!("Doubao resource was not found{detail}"),
        ),
        429 => GatewayError::new(
            ErrorKind::RateLimited,
            format!("Doubao rate limited{detail}"),
        ),
        status if status >= 500 => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("Doubao upstream error {status}{detail}"),
        ),
        status => GatewayError::new(
            ErrorKind::BadRequest,
            format!("Doubao rejected the request ({status}){detail}"),
        ),
    }
}

/// A `200` carrying a JSON error envelope (session expiry takes this shape).
fn classify_payload(value: &Value) -> GatewayError {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let message = value
        .get("msg")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("unexpected body");
    match code {
        0 => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("Doubao returned a non-stream body: {message}"),
        ),
        _ => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("Doubao error {code}: {message}"),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_json_error_envelope_is_not_treated_as_success() {
        let error = classify_payload(&serde_json::json!({"code": 10008, "msg": "no login"}));
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
        assert!(error.message.contains("no login"));
    }

    #[test]
    fn public_parameters_carry_the_imported_device_identity() {
        use crate::accounts::Credential;
        let credential =
            DoubaoCredential::from_credential(&Credential::from_value(serde_json::json!({
                "cookies": "sessionid=s; passport_csrf_token=c",
                "device_id": "d-1", "web_id": "w-1", "fp": "f-1"
            })))
            .expect("parse");
        let http = DoubaoHttp::new(reqwest::Client::new());
        let query: Vec<(String, String)> = http.query(&credential);
        let find = |key: &str| {
            query
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(find("aid").as_deref(), Some(region::APP_ID));
        assert_eq!(find("device_id").as_deref(), Some("d-1"));
        assert_eq!(find("web_id").as_deref(), Some("w-1"));
        assert_eq!(find("fp").as_deref(), Some("f-1"));
        assert_eq!(find("device_platform").as_deref(), Some("web"));
    }
}
