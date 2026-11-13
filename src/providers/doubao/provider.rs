//! Doubao provider: identity, catalogue and inference.
//!
//! The upstream only streams, so a non-streaming call opens the stream and
//! aggregates it. There is no tool surface, no native session id and no cancel
//! endpoint, so `tools`, `native_session`, `agent_task` and `cancellation` stay
//! `false`.
//!
//! The upstream does not report token usage, so [`QuotaSnapshot`] and response
//! `usage` stay unknown/absent rather than carrying an invented estimate.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{stream, StreamExt};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, Provider, ProviderContext, ProviderInfo, QuotaSnapshot, StreamItem,
    UpstreamModel,
};

use super::credential::DoubaoCredential;
use super::events::{aggregate, Event, SseDecoder, StreamTranslator};
use super::http::DoubaoHttp;
use super::payload::{self, Turn};
use super::region;

pub struct DoubaoProvider {
    info: ProviderInfo,
    http: Arc<DoubaoHttp>,
    stream_idle: Duration,
}

impl std::fmt::Debug for DoubaoProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubaoProvider")
            .field("id", &self.info.id)
            .field("base", &self.http.base())
            .finish_non_exhaustive()
    }
}

impl DoubaoProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        Self::build(id, region, verification, DoubaoHttp::new(client))
    }

    /// Tests only: point the client at one local mock server.
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
            DoubaoHttp::with_base(client, base),
        )
    }

    fn build(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        http: DoubaoHttp,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Doubao,
            region,
            display_name: ProviderKind::Doubao.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(http),
            stream_idle: Duration::from_secs(region::STREAM_IDLE_SECONDS),
        }
    }

    fn credential(&self, credential: &Credential) -> Result<DoubaoCredential, GatewayError> {
        DoubaoCredential::from_credential(credential)
    }

    async fn open_stream_with(
        &self,
        credential: DoubaoCredential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let turn = payload::flatten(&request.messages)?;
        let body = payload::build_body(&request, &turn)?;
        let response = self.http.open_stream(&credential, &body).await?;
        let state = StreamState {
            body: response,
            decoder: SseDecoder::new(),
            translator: StreamTranslator::new(request.model.clone()),
            events: VecDeque::new(),
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
impl Provider for DoubaoProvider {
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
            tools: false,
            native_session: false,
            agent_task: false,
            cancellation: false,
        }
    }

    /// Read-only probe: a dead session is answered with a business error inside
    /// an otherwise healthy SSE stream, so the probe opens one tiny stream.
    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        if credential.missing_identity().is_some() {
            return Ok(AuthStatus::Invalid);
        }
        let request = ChatRequest::new("doubao", Vec::new());
        let turn = Turn {
            text: "hi".to_string(),
        };
        let body = payload::build_body(&request, &turn)?;
        match self.http.open_stream(&credential, &body).await {
            Ok(_) => Ok(AuthStatus::Valid),
            Err(error) if error.kind == ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
            Err(error) if error.kind == ErrorKind::AuthInvalid => Ok(AuthStatus::Invalid),
            Err(error) if error.kind == ErrorKind::RateLimited => Ok(AuthStatus::Restricted),
            Err(error) => Err(error),
        }
    }

    /// The upstream has no model catalogue endpoint: the client exposes three
    /// thinking tiers, so those are the models this provider reports.
    async fn quota_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        let value = self.http.get_quota_summary(&credential).await?;
        let account_id = context.account_id.clone().unwrap_or_default();
        Ok(super::quota::parse_summary(&account_id, &value))
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        self.credential(credential)?;
        Ok(region::catalog()
            .into_iter()
            .map(|(id, name, context)| {
                let mut model = UpstreamModel::new(id).with_display_name(name);
                model.context_window = Some(context);
                model
            })
            .collect())
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let credential = self.credential(credential)?;
        let model = request.model.clone();
        let mut stream = self.open_stream_with(credential, request).await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return Err(error),
            }
        }
        let response_id = events
            .iter()
            .find_map(|event| match event {
                crate::protocol::StreamEvent::Start { id, .. } => Some(id.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("doubao-{}", uuid::Uuid::new_v4()));
        aggregate(&response_id, &model, &events)
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
    events: VecDeque<Event>,
    queue: VecDeque<StreamItem>,
    done: bool,
    eof: bool,
    idle: Duration,
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
            if let Some(event) = self.events.pop_front() {
                match self.translator.push(&event) {
                    Ok(items) => {
                        self.queue.extend(items.into_iter().map(Ok));
                        if let Some(item) = self.queue.pop_front() {
                            return Some(item);
                        }
                        if !self.translator.started() {
                            continue;
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
                return self.translator.finish().map(Ok);
            }
            let chunk = match tokio::time::timeout(self.idle, self.body.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => {
                    self.eof = true;
                    if let Ok(events) = self.decoder.finish() {
                        self.events.extend(events);
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
                        "Doubao stream went idle",
                    )));
                }
            };
            match self.decoder.push(&chunk) {
                Ok(events) => self.events.extend(events),
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

    fn provider() -> DoubaoProvider {
        DoubaoProvider::new(
            "doubao-cn",
            Region::Cn,
            Verification::Experimental,
            reqwest::Client::new(),
        )
    }

    #[test]
    fn declared_capabilities_match_the_implemented_surface() {
        let caps = provider().capabilities();
        assert!(caps.authentication && caps.model_discovery);
        assert!(caps.chat && caps.streaming);
        assert!(!caps.tools && !caps.native_session && !caps.cancellation);
        assert!(caps.quota, "the subscription summary endpoint is readable");
    }

    #[test]
    fn the_provider_reports_its_own_identity() {
        let info = provider().info();
        assert_eq!(info.kind, ProviderKind::Doubao);
        assert_eq!(info.region, Region::Cn);
    }
}
