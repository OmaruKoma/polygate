//! TeleAgent provider: identity, catalogue, quota and inference.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md`. The upstream speaks OpenAI chat
//! completions in both directions, so inference is a projection (see
//! [`super::convert`]) wrapped in the `X-SA-*` signature set.
//!
//! There is no refresh token: the JWT lives about 7 days and the operator
//! imports a fresh `<tenant>:<JWT>` when the upstream rejects it, so
//! `auth_status` only probes. No session id, cancel endpoint or agent task
//! surface is implemented, so those capability bits stay false.

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
use super::credential::TeleagentCredential;
use super::events::{parse_frame, Frame, SseDecoder, StreamTranslator};
use super::http::TeleagentHttp;
use super::models::{parse_catalog_with, Limits};
use super::quota::parse_user_center;

/// A stream that produces no bytes for this long is treated as interrupted.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub struct TeleagentProvider {
    info: ProviderInfo,
    http: Arc<TeleagentHttp>,
    stream_idle: std::time::Duration,
    limits: Limits,
    extra_models: Vec<String>,
}

impl std::fmt::Debug for TeleagentProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeleagentProvider")
            .field("id", &self.info.id)
            .field("base", &self.http.base())
            .finish_non_exhaustive()
    }
}

impl TeleagentProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        Self::build(id, region, verification, TeleagentHttp::new(client))
    }

    /// Tests only: point the model gateway at one local mock server.
    pub fn with_base(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self::build(
            id,
            region,
            verification,
            TeleagentHttp::with_base(client, base),
        )
    }

    fn build(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        http: TeleagentHttp,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Teleagent,
            region,
            display_name: ProviderKind::Teleagent.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(http),
            stream_idle: STREAM_IDLE_TIMEOUT,
            limits: Limits::default(),
            extra_models: Vec::new(),
        }
    }

    /// Operator-declared window for models the upstream lists as `null`
    /// (`model.context` / `model.max_output` provider options).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Ids the upstream serves but leaves out of `/model-options`
    /// (`model.extra`, comma separated).
    pub fn with_extra_models(mut self, ids: Vec<String>) -> Self {
        self.extra_models = ids;
        self
    }

    fn credential(&self, credential: &Credential) -> Result<TeleagentCredential, GatewayError> {
        TeleagentCredential::from_credential(credential)
    }

    async fn open_stream_with(
        &self,
        credential: TeleagentCredential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let body = serde_json::to_vec(&convert::build_body(&request, true)?)
            .map_err(|_| GatewayError::internal("TeleAgent body serialization failed"))?;
        let response = self.http.open_stream(&credential, &body).await?;
        let state = StreamState {
            body: Some(response),
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
impl Provider for TeleagentProvider {
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

    /// Read-only probe: the model-options endpoint answers 401 for a dead token.
    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        match self.http.get_model_options(&credential).await {
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
        let value = self.http.get_model_options(&credential).await?;
        parse_catalog_with(&value, self.limits, &self.extra_models)
    }

    async fn quota_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        let value = self.http.get_user_center(&credential).await?;
        let account_id = context.account_id.clone().unwrap_or_default();
        Ok(parse_user_center(&account_id, &value))
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let credential = self.credential(credential)?;
        let body = serde_json::to_vec(&convert::build_body(&request, false)?)
            .map_err(|_| GatewayError::internal("TeleAgent body serialization failed"))?;
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
    body: Option<reqwest::Response>,
    decoder: SseDecoder,
    translator: StreamTranslator,
    frames: VecDeque<String>,
    queue: VecDeque<StreamItem>,
    done: bool,
    eof: bool,
    idle: std::time::Duration,
}

impl StreamState {
    /// Consume whatever is left of the response in the background so hyper can
    /// return the connection to the pool. A dropped, half-read SSE body closes
    /// the TCP connection instead, which forces the next call onto a fresh
    /// connection and — measured — onto one of the upstream's slower replicas.
    fn drain(&mut self) {
        let Some(body) = self.body.take() else {
            return;
        };
        tokio::spawn(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), body.bytes()).await;
        });
    }

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
                        self.drain();
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
                self.drain();
                // The upstream closed without `[DONE]`; report what we saw.
                return self.translator.finish().map(Ok);
            }
            let Some(body) = self.body.as_mut() else {
                self.done = true;
                return self.translator.finish().map(Ok);
            };
            let chunk = match tokio::time::timeout(self.idle, body.chunk()).await {
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
                        "TeleAgent stream went idle",
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

    fn provider() -> TeleagentProvider {
        TeleagentProvider::new(
            "teleagent-cn",
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
        assert_eq!(info.kind, ProviderKind::Teleagent);
        assert_eq!(info.region, Region::Cn);
    }
}
