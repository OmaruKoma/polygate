//! Qoder inference gateway client (COSY-signed requests).
//!
//! Source: Coding2API `src/provider/qoder/client.py` (`list_models`,
//! `stream_chat`, gateway fallback) and `src/provider/qoder/cosy.py` at commit
//! `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! Only two endpoints are signed: `GET /algo/api/v2/model/list?Encode=1` and
//! `POST /algo/api/v2/service/pro/sse/agent_chat_generation`. The chat body is
//! `qoder_encode`d, and the exact encoded string is both sent and signed.
//!
//! Gateway fallback (Intl only) is attempted **before** any response body is
//! consumed and only for transport failures. A response that already started
//! streaming is never replayed: it may have been billed.
//!
//! A fresh [`CosySession`] is built per request. The reference caches it per
//! `(uid, access_token)`; both are equivalent because the session only holds
//! the encrypted identity and the RSA-wrapped ephemeral key, and a rotated
//! token must rebuild it anyway.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::providers::UpstreamModel;

use super::cosy::{self, CosyIdentity, CosySession};
use super::credential::QoderCredential;
use super::error::{classify_http_status, classify_payload, classify_transport};
use super::region;
use crate::providers::profile::Identity;

const MAX_JSON_BYTES: usize = 512 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// One realm's inference gateway. The base override exists for deterministic
/// tests and is never read from production configuration.
#[derive(Clone)]
pub struct QoderGateway {
    client: reqwest::Client,
    bases: Vec<String>,
    identity: Identity,
}

impl std::fmt::Debug for QoderGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderGateway")
            .field("bases", &self.bases)
            .finish_non_exhaustive()
    }
}

impl QoderGateway {
    pub fn new(client: reqwest::Client, region: crate::config::Region) -> Self {
        Self {
            client,
            bases: region::realm(region)
                .gateway_candidates()
                .into_iter()
                .map(|base| base.trim_end_matches('/').to_string())
                .collect(),
            identity: Identity::default(),
        }
    }

    /// Tests only: one host, no fallback.
    /// In-place variant, for callers that own the value through an `Arc`.
    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = identity;
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    pub fn with_base(
        client: reqwest::Client,
        _region: crate::config::Region,
        base: impl Into<String>,
    ) -> Self {
        Self {
            client,
            bases: vec![base.into().trim_end_matches('/').to_string()],
            identity: Identity::default(),
        }
    }

    /// Model catalogue. Requires the full COSY header set; a bearer-token GET
    /// is rejected by the upstream.
    pub async fn list_models(
        &self,
        credential: &QoderCredential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let session = session_for(credential, self.identity)?;
        let mut last_error = None;
        for base in &self.bases {
            let url = format!("{base}{}", region::MODELS_PATH);
            let date = cosy::cosy_date(chrono::Utc::now());
            let request_id = cosy::new_request_id();
            let headers = session.models_headers("", &url, &date, &request_id)?;
            let request = self
                .client
                .get(&url)
                .headers(header_map(headers)?)
                .timeout(HEADER_TIMEOUT);
            match self.send_json(request, &url).await {
                Ok(payload) => return parse_models(&payload),
                Err(error) if retry_on_another_host(&error) => last_error = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| GatewayError::transport("Qoder has no gateway host")))
    }

    /// Open the streaming chat endpoint. The returned response is consumed by
    /// the provider's stream state machine; it is never buffered here.
    pub async fn open_chat(
        &self,
        credential: &QoderCredential,
        model: &str,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        let session = session_for(credential, self.identity)?;
        let encoded = super::convert::encode_body(body)?;
        let mut last_error = None;
        for base in &self.bases {
            let url = format!("{base}{}?{}", region::CHAT_PATH, region::CHAT_QUERY);
            let date = cosy::cosy_date(chrono::Utc::now());
            let request_id = cosy::new_request_id();
            let headers = session.chat_headers(&encoded, &url, model, &date, &request_id)?;
            let request = self
                .client
                .post(&url)
                .headers(header_map(headers)?)
                .body(encoded.clone());
            let response = match tokio::time::timeout(HEADER_TIMEOUT, request.send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let error = classify_transport(&error);
                    if retry_on_another_host(&error) {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
                Err(_) => {
                    last_error = Some(GatewayError::timeout("Qoder chat did not answer in time"));
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status >= 400 {
                let body = read_error_body(response).await?;
                return Err(classify_http_status(status, &body));
            }
            return Ok(response);
        }
        Err(last_error.unwrap_or_else(|| GatewayError::transport("Qoder has no gateway host")))
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
        let bytes = response
            .bytes()
            .await
            .map_err(|error| classify_transport(&error))?;
        if bytes.len() > MAX_JSON_BYTES {
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

/// Bounded read for an error body; a rejected chat never streams it.
async fn read_error_body(mut response: reqwest::Response) -> Result<String, GatewayError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_transport(&error))?
    {
        if body.len() + chunk.len() > MAX_JSON_BYTES {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn session_for(
    credential: &QoderCredential,
    identity: Identity,
) -> Result<CosySession, GatewayError> {
    let cosy = CosyIdentity::new(credential.uid.clone(), credential.access_token.clone())
        .with_name(credential.nickname.clone());
    let mut session = CosySession::new_random(cosy)?;
    session.set_identity(identity);
    Ok(session)
}

fn header_map(headers: Vec<(String, String)>) -> Result<HeaderMap, GatewayError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| GatewayError::internal("invalid Qoder header name"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| GatewayError::internal("invalid Qoder header value"))?;
        map.insert(name, value);
    }
    Ok(map)
}

/// Only a failure that never reached the upstream may be retried on the next
/// host of the same realm.
fn retry_on_another_host(error: &GatewayError) -> bool {
    matches!(error.kind, ErrorKind::Transport | ErrorKind::Timeout)
}

/// Model list mapping. The upstream answers with one array per scene: the
/// desktop client reads `app`, the captured protocol carried `chat`. An absent
/// or empty catalogue is a discovery failure, not "this account has no models".
pub fn parse_models(payload: &Value) -> Result<Vec<UpstreamModel>, GatewayError> {
    let items = scene_items(payload)
        .ok_or_else(|| GatewayError::upstream_server("Qoder model list has no model array"))?;
    let mut models = Vec::new();
    for item in items {
        let Some(key) = item
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        if item.get("enable").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let mut model = UpstreamModel::new(key);
        if let Some(name) = item
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            model = model.with_display_name(name);
        }
        model.context_window = item
            .get("max_input_tokens")
            .and_then(Value::as_u64)
            .or_else(|| item.get("default_context_window").and_then(Value::as_u64));
        model.max_output = item.get("max_output_tokens").and_then(Value::as_u64);
        models.push(model);
    }
    if models.is_empty() {
        return Err(GatewayError::upstream_server(
            "Qoder model list contained no usable model",
        ));
    }
    Ok(models)
}

/// Upstream `is_vl` is a coarse catalogue flag, not a capability fact: on
/// 2026-10-30 `dmodel` answered "Unknown" with an image attached while
/// `kmodel` read the same image correctly. Vision stays on the live-verified
/// allowlist in [`crate::model_service::verified_vision`].
fn scene_items(payload: &Value) -> Option<&[Value]> {
    ["chat", "app"].iter().find_map(|scene| {
        payload
            .get(*scene)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    })
}
