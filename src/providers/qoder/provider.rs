//! Qoder provider: identity, models, quota and inference.
//!
//! Inference is COSY-signed and streaming-only upstream. The non-streaming
//! [`Provider::chat`] aggregates the same stream inside the gateway
//! ([`super::convert::aggregate`]); no second upstream endpoint is invented.
//!
//! There is no upstream session id and no cancellation endpoint, so
//! `native_session`, `agent_task` and `cancellation` stay `false`.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, Provider, ProviderInfo, StreamItem, UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use super::convert;
use super::credential::QoderCredential;
use super::error::classify_transport;
use super::events::{QoderStreamTranslator, SseDecoder, SseFrame};
use super::gateway::QoderGateway;
use super::http::QoderApi;
use super::quota::{self, parse_quota};
use super::region;

/// A stream that produces no bytes for this long is treated as interrupted.
/// The official clients apply an equivalent idle timeout so a stalled upstream
/// cannot hold a session/account lease forever.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub struct QoderProvider {
    info: ProviderInfo,
    api: Arc<QoderApi>,
    gateway: Arc<QoderGateway>,
    stream_idle: std::time::Duration,
}

impl std::fmt::Debug for QoderProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderProvider")
            .field("id", &self.info.id)
            .field("region", &self.info.region)
            .field("base", &self.api.base())
            .finish_non_exhaustive()
    }
}

impl QoderProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Qoder,
            region,
            display_name: format!(
                "{} ({})",
                ProviderKind::Qoder.display_name(),
                region.as_str().to_uppercase()
            ),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            api: Arc::new(QoderApi::new(client.clone(), region)),
            gateway: Arc::new(QoderGateway::new(client, region)),
            stream_idle: STREAM_IDLE_TIMEOUT,
        }
    }

    /// Tests only: both the OpenAPI and the inference host point at one local
    /// mock server.
    pub fn with_base(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        let mut provider = Self::new(id, region, verification, client.clone());
        let base = base.into();
        provider.api = Arc::new(QoderApi::with_base(client.clone(), region, base.clone()));
        provider.gateway = Arc::new(QoderGateway::with_base(client, region, base));
        // `with_base` rebuilds both clients, so the identity has to be carried
        // by the caller through `with_identity` afterwards.
        provider
    }

    /// Send the official desktop client's identity headers upstream.
    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        Arc::make_mut(&mut self.api).set_identity(identity);
        Arc::make_mut(&mut self.gateway).set_identity(identity);
        self
    }

    /// Tests only: shorten the idle timeout so a stalled mock stream is cheap.
    #[cfg(test)]
    pub fn with_stream_idle_timeout(mut self, idle: std::time::Duration) -> Self {
        self.stream_idle = idle;
        self
    }

    fn credential(&self, credential: &Credential) -> Result<QoderCredential, GatewayError> {
        QoderCredential::from_credential(credential, self.info.region)
    }

    /// Non-streamed chat: aggregate the same upstream stream locally.
    pub async fn chat_for(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        let request_id = request.request_id.clone();
        let model = request.model.clone();
        let mut stream = self
            .open_stream(credential, request, response_id.clone())
            .await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item?);
        }
        convert::aggregate(&response_id, request_id, &model, &events)
    }

    pub async fn chat_stream_for(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        self.open_stream(credential, request, response_id).await
    }

    async fn open_stream(
        &self,
        credential: &Credential,
        request: ChatRequest,
        response_id: String,
    ) -> Result<ChatStream, GatewayError> {
        let credential = self.credential(credential)?;
        let body = convert::build_body(&request)?;
        let response = self
            .gateway
            .open_chat(&credential, &request.model, &body)
            .await?;
        let translator = QoderStreamTranslator::new(
            response_id,
            request.request_id.clone(),
            request.model.clone(),
        );
        let state = StreamState {
            body: response,
            decoder: SseDecoder::new(),
            translator,
            frames: VecDeque::new(),
            queue: VecDeque::new(),
            started: false,
            done: false,
            idle: self.stream_idle,
        };
        let events = stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        });
        Ok(Box::pin(events))
    }
}

struct StreamState {
    body: reqwest::Response,
    decoder: SseDecoder,
    translator: QoderStreamTranslator,
    frames: VecDeque<SseFrame>,
    queue: VecDeque<StreamItem>,
    started: bool,
    done: bool,
    idle: std::time::Duration,
}

impl StreamState {
    async fn next(&mut self) -> Option<StreamItem> {
        if let Some(item) = self.queue.pop_front() {
            return Some(item);
        }
        if !self.started {
            self.started = true;
            return Some(Ok(self.translator.start_event()));
        }
        if self.done {
            return None;
        }
        loop {
            if let Some(frame) = self.frames.pop_front() {
                if let Err(error) = self.translate(frame) {
                    self.done = true;
                    return Some(Err(error));
                }
                if let Some(item) = self.queue.pop_front() {
                    return Some(item);
                }
                continue;
            }
            let chunk = match tokio::time::timeout(self.idle, self.body.chunk()).await {
                Ok(result) => result,
                Err(_) => {
                    self.done = true;
                    return Some(Err(GatewayError::timeout(
                        "Qoder stream went idle before the response finished",
                    )));
                }
            };
            match chunk {
                Ok(Some(bytes)) => match self.decoder.push(&bytes) {
                    Ok(frames) => self.frames.extend(frames),
                    Err(error) => {
                        self.done = true;
                        return Some(Err(error));
                    }
                },
                Ok(None) => {
                    self.done = true;
                    match self.decoder.finish() {
                        Ok(frames) => self.frames.extend(frames),
                        Err(error) => self.queue.push_back(Err(error)),
                    }
                    while let Some(frame) = self.frames.pop_front() {
                        if let Err(error) = self.translate(frame) {
                            self.queue.push_back(Err(error));
                            return self.queue.pop_front();
                        }
                    }
                    self.queue
                        .extend(self.translator.end_of_stream().into_iter().map(Ok));
                    return self.queue.pop_front();
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(classify_transport(&error)));
                }
            }
        }
    }

    fn translate(&mut self, frame: SseFrame) -> Result<(), GatewayError> {
        let events = self.translator.translate_frame(&frame)?;
        self.queue.extend(events.into_iter().map(Ok));
        Ok(())
    }
}

#[async_trait]
impl Provider for QoderProvider {
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

    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        if credential.expires_at > 0 && Utc::now().timestamp() >= credential.expires_at {
            return Ok(AuthStatus::Expired);
        }
        match self.api.get(region::USERINFO_PATH, &credential).await {
            Ok(_) => Ok(AuthStatus::Valid),
            Err(error) => match error.kind {
                ErrorKind::AuthExpired => Ok(AuthStatus::RefreshRequired),
                ErrorKind::AuthInvalid => Ok(AuthStatus::Invalid),
                ErrorKind::ChannelRestricted => Ok(AuthStatus::Restricted),
                _ => Err(error),
            },
        }
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let credential = self.credential(credential)?;
        self.gateway.list_models(&credential).await
    }

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        // The presentation route is what the IDE shows; the narrower quota route
        // answers with the same counters, so either alone is usable. A failing or
        // empty first read falls through instead of failing the whole probe.
        if let Ok(payload) = self.api.get(quota::PRESENTATION_PATH, &credential).await {
            let snapshot = parse_quota("", &payload);
            if snapshot.remaining.is_some() {
                return Ok(snapshot);
            }
        }
        let payload = self.api.get(region::QUOTA_PATH, &credential).await?;
        Ok(parse_quota("", &payload))
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        self.chat_for(credential, request).await
    }

    async fn chat_stream(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.chat_stream_for(credential, request).await
    }
}
