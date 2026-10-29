//! Signed HTTP transport for the CodeArts snap engine.
//!
//! Two request classes (evidence: local `client.py`, Coding2API `client.py`):
//! * short JSON calls (`/v1/model/builtin`, `/snap-manager/v1/statistics/plugin`,
//!   `/v1/ops/*`) with `Agent-Type: PromptCenter`;
//! * the chat stream (`/api/v2/chat/completions`) with `Accept:
//!   text/event-stream` and `x-auth-token` (the STS token is also part of the
//!   signature as `X-Security-Token`).
//!
//! Nothing here retries. Retrying, account rotation and session recovery are
//! caller policy.

use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde_json::Value;
use url::Url;

use crate::error::{ErrorKind, GatewayError};

use super::auth::CodeArtsCredential;
use super::error::{classify_http_status, retry_after};
use super::signer::{sign_request, SignCredential};

const MAX_BODY_BYTES: usize = 128 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

pub const CHAT_PATH: &str = "/api/v2/chat/completions";
pub const MODEL_BUILTIN_PATH: &str = "/v1/model/builtin";
pub const QUOTA_PATH: &str = "/snap-manager/v1/statistics/plugin";
pub const STATISTICS_PLUGIN_PATH: &str = QUOTA_PATH;

/// Upstream identity headers. These are documented client constants from the
/// reference implementations, not secrets and not per-account values.
#[derive(Debug, Clone)]
pub struct ClientHeaders {
    pub language: String,
    pub app_id: String,
    pub client_version: String,
}

impl Default for ClientHeaders {
    fn default() -> Self {
        Self {
            language: "zh-cn".into(),
            app_id: "CodeAgent3.0".into(),
            client_version: "26.7.2".into(),
        }
    }
}

#[derive(Clone)]
pub struct CodeArtsHttp {
    http: reqwest::Client,
    base_url: String,
    sts_url: String,
    headers: ClientHeaders,
    short_timeout: Duration,
    stream_headers_timeout: Duration,
}

impl std::fmt::Debug for CodeArtsHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeArtsHttp")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl CodeArtsHttp {
    pub fn new(
        base_url: impl Into<String>,
        sts_url: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Url::parse(&base_url)
            .map_err(|error| GatewayError::config(format!("invalid CodeArts base_url: {error}")))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| GatewayError::config(format!("cannot build HTTP client: {error}")))?;
        Ok(Self {
            http,
            base_url,
            sts_url: sts_url.into().trim_end_matches('/').to_string(),
            headers: ClientHeaders::default(),
            short_timeout: Duration::from_secs(30),
            stream_headers_timeout: Duration::from_secs(60),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn sts_url(&self) -> &str {
        &self.sts_url
    }

    pub fn with_headers(mut self, headers: ClientHeaders) -> Self {
        self.headers = headers;
        self
    }

    /// Overrides for tests and for operators on a slow link.
    pub fn with_timeouts(mut self, short: Duration, stream_headers: Duration) -> Self {
        self.short_timeout = short;
        self.stream_headers_timeout = stream_headers;
        self
    }

    fn url(&self, path: &str) -> Result<Url, GatewayError> {
        Url::parse(&format!("{}{path}", self.base_url))
            .map_err(|error| GatewayError::internal(format!("invalid CodeArts URL: {error}")))
    }

    fn sign(
        &self,
        method: &str,
        url: &Url,
        headers: &[(String, String)],
        body: &[u8],
        credential: &CodeArtsCredential,
        now: DateTime<Utc>,
    ) -> Vec<(String, String)> {
        let signing = SignCredential {
            access_key_id: credential.access_key_id.clone(),
            secret_access_key: credential.secret_access_key.to_string(),
            security_token: credential.security_token.to_string(),
        };
        sign_request(method, url, headers, body, &signing, now).headers
    }

    fn apply_headers(
        request: reqwest::RequestBuilder,
        headers: &[(String, String)],
    ) -> reqwest::RequestBuilder {
        headers.iter().fold(request, |request, (key, value)| {
            // Hyper sets `Host` from the URL itself; an explicit identical value
            // keeps the signed header set and the wire header set aligned.
            request.header(key, value)
        })
    }

    pub async fn get_json(
        &self,
        path: &str,
        credential: &CodeArtsCredential,
        agent_type: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        let url = self.url(path)?;
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Language".to_string(), self.headers.language.clone()),
            (
                "x-snap-traceid".to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
            ),
        ];
        if let Some(agent_type) = agent_type {
            headers.push(("Agent-Type".to_string(), agent_type.to_string()));
        }
        let signed = self.sign("GET", &url, &headers, b"", credential, now);
        let request = Self::apply_headers(self.http.get(url), &signed);
        self.execute_json(request, self.short_timeout).await
    }

    pub async fn post_json(
        &self,
        path: &str,
        body: &Value,
        credential: &CodeArtsCredential,
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        let url = self.url(path)?;
        let raw = serde_json::to_vec(body)
            .map_err(|error| GatewayError::internal(format!("cannot encode request: {error}")))?;
        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Language".to_string(), self.headers.language.clone()),
            (
                "x-snap-traceid".to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
            ),
            ("Agent-Type".to_string(), "PromptCenter".to_string()),
        ];
        let signed = self.sign("POST", &url, &headers, &raw, credential, now);
        let request = Self::apply_headers(self.http.post(url), &signed).body(raw);
        self.execute_json(request, self.short_timeout).await
    }

    async fn execute_json(
        &self,
        request: reqwest::RequestBuilder,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let response = tokio::time::timeout(timeout, request.send())
            .await
            .map_err(|_| GatewayError::timeout("CodeArts request timed out"))?
            .map_err(map_transport)?;
        let status = response.status().as_u16();
        if status >= 400 {
            let retry = retry_after(response.headers());
            let body = read_body(response).await?;
            let mut error = classify_http_status(status, &body);
            error.retry_after = retry;
            return Err(error);
        }
        let body = read_body(response).await?;
        serde_json::from_str(&body)
            .map_err(|_| GatewayError::upstream_server("CodeArts returned a non-JSON response"))
    }

    /// Open the chat stream. Returns only after response headers arrive, so an
    /// HTTP error is still reported as an `Err` (not as a broken stream).
    pub async fn open_chat(
        &self,
        body: &Value,
        credential: &CodeArtsCredential,
        now: DateTime<Utc>,
    ) -> Result<SseBody, GatewayError> {
        let url = self.url(CHAT_PATH)?;
        let raw = serde_json::to_vec(body)
            .map_err(|error| GatewayError::internal(format!("cannot encode request: {error}")))?;
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("X-Language".to_string(), self.headers.language.clone()),
            (
                "x-snap-traceid".to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
            ),
            ("app-id".to_string(), self.headers.app_id.clone()),
            ("is_confidential".to_string(), "false".to_string()),
            (
                "x-auth-token".to_string(),
                credential.security_token.to_string(),
            ),
        ];
        // Local client also sends plugin/client identity headers on chat. They
        // are signed, so they must be identical on the wire and in the canonical
        // request; keep the set small and stable.
        headers.push((
            "client_version".to_string(),
            self.headers.client_version.clone(),
        ));
        let signed = self.sign("POST", &url, &headers, &raw, credential, now);
        let request = Self::apply_headers(self.http.post(url), &signed).body(raw);
        let response = tokio::time::timeout(self.stream_headers_timeout, request.send())
            .await
            .map_err(|_| GatewayError::timeout("CodeArts chat did not answer in time"))?
            .map_err(map_transport)?;
        let status = response.status().as_u16();
        if status >= 400 {
            let retry = retry_after(response.headers());
            let body = read_body(response).await?;
            let mut error = classify_http_status(status, &body);
            error.retry_after = retry;
            return Err(error);
        }
        Ok(SseBody::new(response))
    }

    pub async fn refresh(
        &self,
        credential: &CodeArtsCredential,
        original: &crate::accounts::Credential,
        now: DateTime<Utc>,
    ) -> Result<crate::accounts::Credential, GatewayError> {
        credential
            .refresh(&self.http, &self.sts_url, original, now)
            .await
    }
}

fn map_transport(error: reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::timeout("CodeArts request timed out")
    } else if error.is_connect() {
        GatewayError::transport("cannot connect to CodeArts")
    } else {
        GatewayError::transport("CodeArts transport error")
    }
}

async fn read_body(response: reqwest::Response) -> Result<String, GatewayError> {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_transport)?;
        let remaining = MAX_BODY_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Raw line reader for the upstream SSE body. CodeArts separates frames with an
/// empty line but also tolerates bare JSON lines, so consumers parse per line.
pub struct SseBody {
    stream: reqwest::Response,
    buffer: Vec<u8>,
    done: bool,
}

impl SseBody {
    fn new(response: reqwest::Response) -> Self {
        Self {
            stream: response,
            buffer: Vec::new(),
            done: false,
        }
    }

    pub async fn next_line(&mut self) -> Result<Option<String>, GatewayError> {
        loop {
            if let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=position).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            if self.done {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let tail = std::mem::take(&mut self.buffer);
                return Ok(Some(String::from_utf8_lossy(&tail).into_owned()));
            }
            match self.stream.chunk().await {
                Ok(Some(chunk)) => {
                    if self.buffer.len() + chunk.len() > MAX_SSE_LINE_BYTES {
                        return Err(GatewayError::new(
                            ErrorKind::UpstreamServer,
                            "CodeArts stream line is too large",
                        ));
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Ok(None) => self.done = true,
                Err(error) => {
                    if error.is_timeout() {
                        return Err(GatewayError::timeout("CodeArts stream timed out"));
                    }
                    if error.is_body() || error.is_decode() {
                        return Err(GatewayError::transport("CodeArts stream was interrupted"));
                    }
                    return Err(map_transport(error));
                }
            }
        }
    }
}

impl std::fmt::Debug for SseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseBody")
            .field("buffered", &self.buffer.len())
            .field("done", &self.done)
            .finish()
    }
}
