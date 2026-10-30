//! Loomy provider: identity, catalogue, quota and inference.
//!
//! The model gateway is OpenAI-shaped, so inference is a projection both ways
//! (see [`super::convert`]). There is no upstream session id and no cancel
//! endpoint, so `native_session`, `agent_task` and `cancellation` stay `false`.
//! There is no refresh token either: the session is valid for 14 days and the
//! operator imports a new one when it expires, so `auth_status` only probes and
//! never rotates a credential.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, Provider, ProviderContext, ProviderInfo, StreamItem, UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use super::convert;
use super::credential::LoomyCredential;
use super::events::{parse_frame, Frame, SseDecoder, StreamTranslator};
use super::http::LoomyHttp;
use super::models::parse_catalog;
use super::quota::parse_points;

/// A stream that produces no bytes for this long is treated as interrupted.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub struct LoomyProvider {
    info: ProviderInfo,
    http: Arc<LoomyHttp>,
    stream_idle: std::time::Duration,
}

impl std::fmt::Debug for LoomyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoomyProvider")
            .field("id", &self.info.id)
            .field("base", &self.http.base())
            .finish_non_exhaustive()
    }
}

impl LoomyProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        Self::build(id, region, verification, LoomyHttp::new(client))
    }

    /// Tests only: point the model gateway at one local mock server.
    pub fn with_base(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self::build(id, region, verification, LoomyHttp::with_base(client, base))
    }

    fn build(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        http: LoomyHttp,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Loomy,
            region,
            display_name: ProviderKind::Loomy.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(http),
            stream_idle: STREAM_IDLE_TIMEOUT,
        }
    }

    fn credential(&self, credential: &Credential) -> Result<LoomyCredential, GatewayError> {
        LoomyCredential::from_credential(credential)
    }

    async fn open_stream_with(
        &self,
        credential: LoomyCredential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let body = convert::build_body(&request, true)?;
        let response = self.http.open_stream(&credential, &body).await?;
        let state = StreamState {
            body: response,
            decoder: SseDecoder::new(),
            translator: StreamTranslator::new(request.model.clone()),
            frames: VecDeque::new(),
            queue: VecDeque::new(),
            done: false,
            eof: false,
            idle: self.stream_idle,
        };
        let events = stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        });
        Ok(Box::pin(events))
    }
}

#[async_trait]
impl Provider for LoomyProvider {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            authentication: true,
            model_discovery: true,
            quota: true,
            chat: true,
            streaming: true,
            tools: true,
            native_session: false,
            agent_task: false,
            cancellation: false,
        }
    }

    /// Read-only probe: the catalogue endpoint answers 401 for a dead session.
    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        match self.http.get_models(&credential).await {
            Ok(_) => Ok(AuthStatus::Valid),
            Err(error) if error.kind == ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
            Err(error) if error.kind == ErrorKind::AuthInvalid => Ok(AuthStatus::Invalid),
            Err(error) if error.kind == ErrorKind::RateLimited => Ok(AuthStatus::Restricted),
            Err(error) => Err(error),
        }
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let credential = self.credential(credential)?;
        let value = self.http.get_models(&credential).await?;
        parse_catalog(&value)
    }

    async fn quota_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        let value = self.http.get_points(&credential, 1).await?;
        let account_id = context.account_id.clone().unwrap_or_default();
        parse_points(&account_id, &value)
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let credential = self.credential(credential)?;
        let body = convert::build_body(&request, false)?;
        let value = self.http.post_chat_json(&credential, &body).await?;
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        convert::parse_response(id, &request.model, &value)
    }

    async fn chat_stream(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let credential = self.credential(credential)?;
        self.open_stream_with(credential, request).await
    }
}

struct StreamState {
    body: reqwest::Response,
    decoder: SseDecoder,
    translator: StreamTranslator,
    frames: VecDeque<String>,
    queue: VecDeque<StreamItem>,
    done: bool,
    eof: bool,
    idle: std::time::Duration,
}

impl StreamState {
    async fn next(&mut self) -> Option<StreamItem> {
        if let Some(item) = self.queue.pop_front() {
            return Some(item);
        }
        if self.done {
            return None;
        }
        loop {
            if let Some(frame) = self.frames.pop_front() {
                match parse_frame(&frame) {
                    Ok(Frame::Done) => {
                        self.done = true;
                        return self.translator.finish().map(Ok);
                    }
                    Ok(Frame::Data(value)) => {
                        self.queue
                            .extend(self.translator.push(&value).into_iter().map(Ok));
                        if let Some(item) = self.queue.pop_front() {
                            return Some(item);
                        }
                        continue;
                    }
                    Err(error) => {
                        self.done = true;
                        return Some(Err(error));
                    }
                }
            }
            if self.eof {
                self.done = true;
                // The upstream closed without `[DONE]`; report what we saw.
                return self.translator.finish().map(Ok);
            }
            let chunk = match tokio::time::timeout(self.idle, self.body.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => {
                    self.eof = true;
                    if let Ok(frames) = self.decoder.finish() {
                        self.frames.extend(frames);
                    }
                    continue;
                }
                Ok(Err(error)) => {
                    self.done = true;
                    return Some(Err(super::http::classify_transport(error)));
                }
                Err(_) => {
                    self.done = true;
                    return Some(Err(GatewayError::new(
                        ErrorKind::Timeout,
                        "Loomy stream went idle",
                    )));
                }
            };
            match self.decoder.push(&chunk) {
                Ok(frames) => self.frames.extend(frames),
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> LoomyProvider {
        LoomyProvider::new(
            "loomy-cn",
            Region::Cn,
            Verification::Experimental,
            reqwest::Client::new(),
        )
    }

    #[test]
    fn declared_capabilities_match_the_implemented_surface() {
        let caps = provider().capabilities();
        assert!(caps.authentication && caps.model_discovery && caps.quota);
        assert!(caps.chat && caps.streaming && caps.tools);
        assert!(!caps.native_session && !caps.agent_task && !caps.cancellation);
    }

    #[test]
    fn the_provider_reports_its_own_identity() {
        let info = provider().info();
        assert_eq!(info.kind, ProviderKind::Loomy);
        assert_eq!(info.region, Region::Cn);
    }
}
