//! OfficeAce HTTP transport.
//!
//! Chat and the model catalogue use `Authorization: Basic` and never expire.
//! Quota and check-in use a `V11-HMAC-SHA256` signature over a temporary
//! AK/SK. Control-plane calls under `/v1/claw/*` use `SDK-HMAC-SHA256`.
//!
//! Nothing here retries: rotation, fallback and cooling are caller policy.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use url::Url;

use crate::error::{ErrorKind, GatewayError};

use super::credential::OfficeAceCredential;
use super::region;
use super::signer::{v11_headers, v4_headers, SignCredential};

const MAX_ERROR_BODY_BYTES: usize = 2048;
const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct OfficeAceHttp {
    client: reqwest::Client,
    cloud: String,
    sts: String,
    region: String,
    short_timeout: Duration,
}

impl std::fmt::Debug for OfficeAceHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfficeAceHttp")
            .field("cloud", &self.cloud)
            .field("sts", &self.sts)
            .finish_non_exhaustive()
    }
}

impl OfficeAceHttp {
    pub fn new(client: reqwest::Client) -> Self {
        Self::with_endpoints(client, region::CLOUD_HOST, region::STS_HOST)
    }

    /// Tests and operators with a private gateway.
    pub fn with_endpoints(
        client: reqwest::Client,
        cloud: impl Into<String>,
        sts: impl Into<String>,
    ) -> Self {
        Self {
            client,
            cloud: cloud.into().trim_end_matches('/').to_string(),
            sts: sts.into().trim_end_matches('/').to_string(),
            region: region::SIGN_REGION.to_string(),
            short_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn cloud(&self) -> &str {
        &self.cloud
    }

    pub fn sts(&self) -> &str {
        &self.sts
    }

    fn gateway(&self, credential: &OfficeAceCredential) -> Result<Url, GatewayError> {
        Url::parse(&credential.gateway()).map_err(|error| {
            GatewayError::new(
                ErrorKind::AuthInvalid,
                format!("stored OfficeAce gateway address is invalid: {error}"),
            )
        })
    }

    // ------------------------------------------------------------ chat

    /// Open the SSE chat stream. A JSON error envelope with HTTP 200 is
    /// classified here so the caller only ever sees a stream.
    pub async fn open_stream(
        &self,
        credential: &OfficeAceCredential,
        body: &Value,
    ) -> Result<reqwest::Response, GatewayError> {
        let url = self
            .gateway(credential)?
            .join(region::EP_CHAT)
            .map_err(|error| GatewayError::internal(format!("invalid chat URL: {error}")))?;
        let response = self
            .client
            .post(url)
            .header("Authorization", credential.authorization())
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(body)
            .send()
            .await
            .map_err(classify_transport)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
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

    // -------------------------------------------------------- catalogue

    pub async fn get_models(
        &self,
        credential: &OfficeAceCredential,
    ) -> Result<Value, GatewayError> {
        let url = self
            .gateway(credential)?
            .join(region::EP_MODELS)
            .map_err(|error| GatewayError::internal(format!("invalid models URL: {error}")))?;
        let request = self
            .client
            .get(url)
            .header("Authorization", credential.authorization())
            .header("Accept", "application/json")
            .timeout(self.short_timeout);
        self.send_json(request).await
    }

    // ---------------------------------------------------- control plane

    pub async fn get_subscription(
        &self,
        credential: &OfficeAceCredential,
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        self.v11_json(
            credential,
            "GET",
            region::EP_SUBSCRIPTION,
            &[("x-subscription-type".into(), "v2".into())],
            b"",
            now,
        )
        .await
    }

    /// Claim today's bonus. The empty JSON object is mandatory: a bare POST
    /// answers 500 `OfficeAce.11020001`. Its bytes also go into the signature.
    pub async fn claim_bonus(
        &self,
        credential: &OfficeAceCredential,
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        self.v11_json(
            credential,
            "POST",
            region::EP_BONUS_CLAIM,
            &[
                ("x-subscription-type".into(), "v2".into()),
                (
                    "Content-Type".into(),
                    "application/json;charset=utf8".into(),
                ),
            ],
            b"{}",
            now,
        )
        .await
    }

    /// Sign a control-plane call with the temporary AK/SK. This runs before
    /// the chat app key exists, so it must not require a full credential.
    pub async fn permission_validate(
        &self,
        credential: &SignCredential,
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        let url = Url::parse(&format!("{}{}", self.cloud, region::EP_CLIENT_PERMISSION)).map_err(
            |error| GatewayError::internal(format!("invalid control-plane URL: {error}")),
        )?;
        let headers = v4_headers(
            credential,
            "GET",
            &url,
            &[(
                "Content-Type".into(),
                "application/json;charset=utf8".into(),
            )],
            b"",
            now,
        );
        let request = self.client.get(url).headers(header_map(&headers));
        self.send_json(request).await
    }

    pub async fn auth_state(&self) -> Result<String, GatewayError> {
        let url = format!("{}{}", self.cloud, region::EP_AUTH_STATE);
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body("{}")
            .timeout(self.short_timeout)
            .send()
            .await
            .map_err(classify_transport)?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = limited_body(response).await?;
            return Err(GatewayError::new(
                ErrorKind::UpstreamServer,
                format!(
                    "OfficeAce refused a login state request: {body}{}",
                    retry_hint(status)
                ),
            ));
        }
        let value = limited_json(response).await?;
        value
            .get("state")
            .and_then(Value::as_str)
            .filter(|state| !state.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                GatewayError::upstream_server("OfficeAce login state response has no state")
            })
    }

    /// `None` means "authorization not finished yet".
    pub async fn auth_code(&self, state: &str) -> Result<Option<String>, GatewayError> {
        let mut request_url = Url::parse(&format!("{}{}", self.cloud, region::EP_AUTH_CODE))
            .map_err(|error| GatewayError::internal(format!("invalid auth code URL: {error}")))?;
        request_url.query_pairs_mut().append_pair("state", state);
        let response = self
            .client
            .get(request_url)
            .header("Accept", "application/json")
            .timeout(self.short_timeout)
            .send()
            .await
            .map_err(classify_transport)?;
        match response.status().as_u16() {
            202 => return Ok(None),
            404 => {
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "OfficeAce authorization code expired or is unknown; start a new login",
                ))
            }
            status if status >= 500 || status == 429 => {
                // A transient upstream answer must not kill the flow.
                return Ok(None);
            }
            status if status >= 400 => {
                let body = limited_body(response).await?;
                return Err(classify_status(status, &body));
            }
            _ => {}
        }
        let value = limited_json(response).await?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            let detail = value
                .get("error_msg")
                .and_then(Value::as_str)
                .unwrap_or(error);
            return Err(GatewayError::new(
                ErrorKind::AuthInvalid,
                format!("OfficeAce authorization was rejected: {detail}"),
            ));
        }
        for key in ["code", "authorization_code"] {
            if let Some(code) = value.get(key).and_then(Value::as_str) {
                if !code.trim().is_empty() {
                    return Ok(Some(code.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// Exchange an authorization code or a refresh token for temporary AK/SK.
    pub async fn token_request(
        &self,
        form: &[(&str, String)],
        dpop_proof: String,
    ) -> Result<Value, GatewayError> {
        let url = format!("{}{}", self.sts, region::EP_OAUTH_TOKENS);
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("DPoP", dpop_proof)
            .form(form)
            .timeout(self.short_timeout)
            .send()
            .await
            .map_err(classify_transport)?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(classify_transport)?;
        if status >= 400 {
            return Err(classify_token_failure(status, &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            GatewayError::upstream_server("OfficeAce token endpoint returned a non-JSON body")
        })
    }

    // ------------------------------------------------------------- inner

    async fn v11_json(
        &self,
        credential: &OfficeAceCredential,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Value, GatewayError> {
        if !credential.has_control_plane() {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "OfficeAce account has no temporary AK/SK; re-authorize it",
            ));
        }
        let url = self
            .gateway(credential)?
            .join(path)
            .map_err(|error| GatewayError::internal(format!("invalid gateway URL: {error}")))?;
        let signed = v11_headers(
            &sign_credential(credential),
            method,
            &url,
            headers,
            body,
            now,
            &self.region,
        );
        let mut request = self
            .client
            .request(method_from(method), url)
            .headers(header_map(&signed))
            .body(body.to_vec());
        request = request.timeout(self.short_timeout);
        let response = request.send().await.map_err(classify_transport)?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = limited_body(response).await?;
            return Err(classify_status(status, &body));
        }
        limited_json(response).await
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

fn method_from(method: &str) -> reqwest::Method {
    reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET)
}

fn sign_credential(credential: &OfficeAceCredential) -> SignCredential {
    SignCredential {
        access_key_id: credential.access_key_id.to_string(),
        secret_access_key: credential.secret_access_key.to_string(),
        security_token: credential.security_token.to_string(),
        project_id: credential.project_id.clone(),
    }
}

fn header_map(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (key, value) in headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            map.insert(name, value);
        }
    }
    map
}

async fn limited_json(response: reqwest::Response) -> Result<Value, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(GatewayError::upstream_server(
            "OfficeAce returned an oversized JSON body",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| GatewayError::upstream_server("OfficeAce returned a body that is not JSON"))
}

async fn limited_body(response: reqwest::Response) -> Result<String, GatewayError> {
    let bytes = response.bytes().await.map_err(classify_transport)?;
    let slice = &bytes[..bytes.len().min(MAX_ERROR_BODY_BYTES)];
    Ok(String::from_utf8_lossy(slice).to_string())
}

pub fn classify_transport(error: reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        GatewayError::new(ErrorKind::Timeout, "OfficeAce request timed out")
    } else {
        GatewayError::new(ErrorKind::Transport, "OfficeAce request failed")
    }
}

fn retry_hint(status: u16) -> String {
    if (500..=599).contains(&status) {
        format!(" (HTTP {status})")
    } else {
        String::new()
    }
}

/// Business codes decide before the HTTP status: `81004`/`81009` arrive as
/// 403/404 but only mean "this account cannot use this model". Treating them as
/// `401` would disable a healthy credential.
fn classify_status(status: u16, body: &str) -> GatewayError {
    let lowered = body.to_ascii_lowercase();
    let detail = summarize(body);
    if lowered.contains("81004") || lowered.contains("81009") {
        return GatewayError::new(
            ErrorKind::ModelUnavailable,
            format!("OfficeAce account cannot use this model{detail}"),
        );
    }
    if ["81111", "81112", "81114"]
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return GatewayError::new(
            ErrorKind::RateLimited,
            format!("OfficeAce rate limited this model{detail}"),
        );
    }
    if lowered.contains("81113") || lowered.contains("81001") {
        return GatewayError::new(
            ErrorKind::BadRequest,
            format!("OfficeAce rejected the request shape{detail}"),
        );
    }
    if lowered.contains("apig.1009")
        || lowered.contains("appkey or appsecret is invalid")
        || lowered.contains("apig.0303")
    {
        return GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("OfficeAce rejected the app key{detail}"),
        );
    }
    if lowered.contains("insufficient balance")
        || lowered.contains("余额不足")
        || lowered.contains("points exhausted")
        || body.contains("14018")
    {
        return GatewayError::new(
            ErrorKind::QuotaExhausted,
            format!("OfficeAce reports exhausted points{detail}"),
        );
    }
    match status {
        402 => GatewayError::new(
            ErrorKind::QuotaExhausted,
            format!("OfficeAce reports exhausted points{detail}"),
        ),
        429 => GatewayError::new(
            ErrorKind::RateLimited,
            format!("OfficeAce rate limited the request{detail}"),
        ),
        401 | 403 => GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("OfficeAce rejected the credential{detail}"),
        ),
        404 => GatewayError::new(
            ErrorKind::ModelUnavailable,
            format!("OfficeAce resource not found{detail}"),
        ),
        400 | 422 => GatewayError::new(
            ErrorKind::BadRequest,
            format!("OfficeAce rejected the request{detail}"),
        ),
        status if status >= 500 => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("OfficeAce upstream error {status}{detail}"),
        ),
        status => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("OfficeAce answered HTTP {status}{detail}"),
        ),
    }
}

fn classify_payload(value: &Value) -> GatewayError {
    let message = value
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("OfficeAce returned an error envelope");
    classify_status(200, message)
}

fn classify_token_failure(status: u16, body: &[u8]) -> GatewayError {
    let text = String::from_utf8_lossy(body);
    let lowered = text.to_ascii_lowercase();
    let terminal = [
        "invalid refresh token",
        "the refresh token has been used",
        "invalid client id",
        "invaliddpopheader",
        "invalid_grant",
        "sts5.1806",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));
    if terminal {
        return GatewayError::new(
            ErrorKind::AuthInvalid,
            "OfficeAce refresh token can no longer be used; re-login is required",
        );
    }
    if status == 429 {
        return GatewayError::new(
            ErrorKind::RateLimited,
            "OfficeAce token endpoint is rate limited",
        );
    }
    if (500..=599).contains(&status) {
        return GatewayError::upstream_server(format!("OfficeAce token endpoint HTTP {status}"));
    }
    GatewayError::new(
        ErrorKind::AuthExpired,
        format!("OfficeAce token exchange failed with HTTP {status}"),
    )
}

fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let clipped: String = trimmed.chars().take(200).collect();
    format!(": {clipped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_permission_error_is_not_an_account_failure() {
        let error = classify_status(
            403,
            r#"{"code":"ModelArts.81004","message":"no permission"}"#,
        );
        assert_eq!(error.kind, ErrorKind::ModelUnavailable);
    }

    #[test]
    fn an_invalid_app_key_is_an_account_failure() {
        let error = classify_status(
            401,
            r#"{"error":"APIG.1009 AppKey or AppSecret is invalid"}"#,
        );
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[test]
    fn exhausted_points_map_to_quota_exhausted() {
        let error = classify_status(400, r#"{"code":"14018","message":"insufficient balance"}"#);
        assert_eq!(error.kind, ErrorKind::QuotaExhausted);
    }

    #[test]
    fn a_used_refresh_token_is_terminal_but_a_5xx_is_not() {
        let terminal = classify_token_failure(
            400,
            br#"{"error":"STS5.1806 the refresh token has been used"}"#,
        );
        assert_eq!(terminal.kind, ErrorKind::AuthInvalid);
        let retryable = classify_token_failure(503, b"");
        assert_eq!(retryable.kind, ErrorKind::UpstreamServer);
    }
}
