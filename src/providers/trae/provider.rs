//! TRAE provider: identity, catalogue, inference, quota and refresh.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §8-§12.
//!
//! TRAE's chat endpoint is streaming-only and uses its own named SSE events;
//! [`Provider::chat`] aggregates the same stream locally. There is no upstream
//! session id and no cancellation endpoint, so `native_session`,
//! `agent_task` and `cancellation` stay `false`.
//!
//! M14-E adds the credential lifecycle (expiry check, ExchangeToken refresh,
//! rotation, revision CAS, cross-process lease) and quota. Check-in lives in
//! [`crate::checkin::trae`].

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use parking_lot::Mutex;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, CredentialStore, Provider, ProviderInfo, StreamItem, UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use super::convert;
use super::credential::TraeCredential;
use super::error::classify_transport;
use super::events::{SseDecoder, SseFrame, StreamTranslator};
use super::http::TraeHttp;
use super::models::{models_request_body, parse_catalog};
use super::quota::{parse_pay_status, parse_usage, usage_request_body};
use super::region;

/// A stream that produces no bytes for this long is treated as interrupted.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Refresh this long before the access token expires.
const REFRESH_SKEW_SECONDS: i64 = 300;
/// Cross-process lease lifetime for one refresh call.
const REFRESH_LEASE_SECONDS: i64 = 60;
/// While another process holds the refresh lease, poll for its result.
const REFRESH_WAIT_STEP: Duration = Duration::from_millis(250);
const REFRESH_WAIT_STEPS: usize = 20;

pub struct TraeProvider {
    info: ProviderInfo,
    http: Arc<TraeHttp>,
    stream_idle: Duration,
    store: Option<Arc<dyn CredentialStore>>,
    refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Identifies this process in the shared refresh lease.
    refresh_owner: String,
    refresh_wait_steps: usize,
    refresh_wait_step: Duration,
    /// Operator-configured numeric check-in device id. Used for pay/check-in
    /// requests only; never generated.
    ug_device_id: Option<String>,
}

impl std::fmt::Debug for TraeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraeProvider")
            .field("id", &self.info.id)
            .field("region", &self.info.region)
            .field("base", &self.http.base())
            .finish_non_exhaustive()
    }
}

impl TraeProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Trae,
            region,
            display_name: format!(
                "{} ({})",
                ProviderKind::Trae.display_name(),
                region.as_str().to_uppercase()
            ),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(TraeHttp::new(client, region)),
            stream_idle: STREAM_IDLE_TIMEOUT,
            store: None,
            refresh_locks: Mutex::new(HashMap::new()),
            refresh_owner: uuid::Uuid::new_v4().to_string(),
            refresh_wait_steps: REFRESH_WAIT_STEPS,
            refresh_wait_step: REFRESH_WAIT_STEP,
            ug_device_id: None,
        }
    }

    /// Send the official IDE's identity headers upstream.
    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        Arc::make_mut(&mut self.http).set_identity(identity);
        self
    }

    pub fn with_credentials_store(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Operator-supplied numeric device id for pay/check-in endpoints. Missing
    /// means the adapter omits the header rather than fabricating one.
    pub fn with_ug_device_id(mut self, device_id: Option<String>) -> Self {
        self.ug_device_id = device_id.filter(|value| !value.trim().is_empty());
        self
    }

    /// Tests only: point the realm at one local mock server.
    pub fn with_base(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        let mut provider = Self::new(id, region, verification, client.clone());
        provider.http = Arc::new(TraeHttp::with_base(client, region, base));
        provider
    }

    #[cfg(test)]
    pub fn with_timeouts(mut self, request: Duration, idle: Duration) -> Self {
        let client = self.http.client().clone();
        let identity = self.http.identity();
        self.http = Arc::new(
            TraeHttp::with_base(client, self.info.region, self.http.base())
                .with_request_timeout(request),
        );
        Arc::make_mut(&mut self.http).set_identity(identity);
        self.stream_idle = idle;
        self
    }

    /// Tests only: shorten the wait for another process's refresh.
    #[cfg(test)]
    pub fn with_refresh_wait(mut self, steps: usize, step: Duration) -> Self {
        self.refresh_wait_steps = steps;
        self.refresh_wait_step = step;
        self
    }

    fn credential(&self, credential: &Credential) -> Result<TraeCredential, GatewayError> {
        TraeCredential::from_credential(credential, self.info.region)
    }

    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }

    fn refresh_lock(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.refresh_locks.lock();
        Arc::clone(
            locks
                .entry(account_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Refresh an expiring credential once per account and persist it with CAS.
    ///
    /// Without an account id the refresh cannot be persisted, so an expiring
    /// credential is used as-is and an expired one is reported honestly.
    async fn credential_for(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
    ) -> Result<TraeCredential, GatewayError> {
        self.credential_for_with(context, credential, false).await
    }

    /// `force` refreshes even when the access token looks valid; it is used for
    /// a single retry after the upstream rejected an otherwise unexpired token.
    async fn credential_for_with(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
        force: bool,
    ) -> Result<TraeCredential, GatewayError> {
        let parsed = self.credential(credential)?;
        let now = self.now();
        if !force && !parsed.needs_refresh(now, REFRESH_SKEW_SECONDS) {
            return Ok(parsed);
        }
        let expired = parsed.is_expired(now);
        let (Some(store), Some(account_id)) = (self.store.as_ref(), context.account_id.as_ref())
        else {
            if expired {
                return Err(GatewayError::new(
                    ErrorKind::AuthExpired,
                    "TRAE credential expired; no credential store is available to persist a refresh",
                ));
            }
            return Ok(parsed);
        };
        if !parsed.can_refresh() {
            if expired {
                return Err(GatewayError::new(
                    ErrorKind::AuthExpired,
                    "TRAE credential expired and has no usable refresh token",
                ));
            }
            return Ok(parsed);
        }

        let lock = self.refresh_lock(account_id);
        let _guard = lock.lock_owned().await;

        // Another request may have refreshed while this one waited.
        let (latest, revision) = match store.load_credential(account_id).await? {
            Some(value) => value,
            None => (credential.clone(), context.credential_revision.unwrap_or(0)),
        };
        let latest = self.credential(&latest)?;
        let now = self.now();
        if !force && !latest.needs_refresh(now, REFRESH_SKEW_SECONDS) {
            return Ok(latest);
        }
        if !latest.can_refresh() {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "TRAE credential expired and has no usable refresh token",
            ));
        }

        match store
            .acquire_refresh_lease(account_id, &self.refresh_owner, now, REFRESH_LEASE_SECONDS)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return self
                    .wait_for_other_refresh(store.as_ref(), account_id)
                    .await
            }
            Err(error) => return Err(error),
        }

        let refreshed = match self.http.refresh(&latest, now).await {
            Ok(credential) => credential,
            Err(error) => {
                let _ = store
                    .release_refresh_lease(account_id, &self.refresh_owner)
                    .await;
                return Err(error);
            }
        };
        let secret = Credential::from_value(refreshed.to_json());
        let outcome = store.update_credential(account_id, revision, &secret).await;
        // Release only after the write: a second process must reload the new
        // value, not replay the rotated refresh token.
        let _ = store
            .release_refresh_lease(account_id, &self.refresh_owner)
            .await;
        match outcome {
            Ok(_) => Ok(refreshed),
            Err(update_error) => {
                // A concurrent writer won. If it stored a fresh credential use
                // that; never refresh twice, the refresh token may have rotated.
                if let Some((stored, _)) = store.load_credential(account_id).await? {
                    let stored = self.credential(&stored)?;
                    if !stored.needs_refresh(self.now(), REFRESH_SKEW_SECONDS) {
                        return Ok(stored);
                    }
                }
                if update_error.kind == ErrorKind::Storage {
                    return Err(update_error);
                }
                Err(GatewayError::new(
                    ErrorKind::AuthExpired,
                    "TRAE credential refresh was not persisted; authorize the account again",
                ))
            }
        }
    }

    /// Another process owns the refresh lease. Poll for its persisted result
    /// instead of sending a second refresh with the same rotating token.
    async fn wait_for_other_refresh(
        &self,
        store: &dyn CredentialStore,
        account_id: &str,
    ) -> Result<TraeCredential, GatewayError> {
        for _ in 0..self.refresh_wait_steps {
            tokio::time::sleep(self.refresh_wait_step).await;
            if let Some((stored, _)) = store.load_credential(account_id).await? {
                let stored = self.credential(&stored)?;
                if !stored.needs_refresh(self.now(), REFRESH_SKEW_SECONDS) {
                    return Ok(stored);
                }
            }
        }
        Err(GatewayError::timeout(
            "another PolyGate process is refreshing this TRAE account; retry shortly",
        ))
    }

    async fn quota_with(&self, credential: &TraeCredential) -> Result<QuotaSnapshot, GatewayError> {
        let now = self.now();
        match self.info.region {
            Region::Cn => {
                let payload = self
                    .http
                    .post_pay_json(
                        region::USAGE_PATH,
                        &usage_request_body(),
                        credential,
                        self.ug_device_id.as_deref(),
                    )
                    .await?;
                parse_usage(self.info.region, &payload, now)
            }
            Region::Intl => {
                let payload = self
                    .http
                    .post_pay_json(
                        region::PAY_STATUS_PATH,
                        &super::quota::pay_status_request_body(),
                        credential,
                        self.ug_device_id.as_deref(),
                    )
                    .await?;
                Ok(parse_pay_status(&payload))
            }
        }
    }

    /// Refuse to spend a request on a credential we know is dead. The
    /// context-aware entry points refresh first; this is the no-context fallback.
    fn ensure_usable(&self, credential: &TraeCredential) -> Result<(), GatewayError> {
        if credential.is_expired(self.now()) {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "TRAE credential expired; refresh or re-authorize the account",
            ));
        }
        Ok(())
    }

    pub async fn chat_for(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let credential = self.credential(credential)?;
        self.ensure_usable(&credential)?;
        self.chat_with(credential, request).await
    }

    async fn chat_with(
        &self,
        credential: TraeCredential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        let request_id = request.request_id.clone();
        let model = request.model.clone();
        let mut stream = self
            .open_stream_with(credential, request, response_id.clone())
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
        let credential = self.credential(credential)?;
        self.ensure_usable(&credential)?;
        let response_id = uuid::Uuid::new_v4().to_string();
        self.open_stream_with(credential, request, response_id)
            .await
    }

    async fn open_stream_with(
        &self,
        credential: TraeCredential,
        request: ChatRequest,
        response_id: String,
    ) -> Result<ChatStream, GatewayError> {
        let body = convert::build_body(&request)?;
        let response = self.http.open_stream(&credential, &body).await?;
        let translator =
            StreamTranslator::new(response_id, request.request_id.clone(), request.model);
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
    translator: StreamTranslator,
    frames: VecDeque<SseFrame>,
    queue: VecDeque<StreamItem>,
    started: bool,
    done: bool,
    idle: Duration,
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
                match self.translator.translate(&frame.event, &frame.data) {
                    Ok(events) => {
                        self.queue.extend(events.into_iter().map(Ok));
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
            let chunk = match tokio::time::timeout(self.idle, self.body.chunk()).await {
                Ok(result) => result,
                Err(_) => {
                    self.done = true;
                    return Some(Err(GatewayError::timeout(
                        "TRAE stream went idle before the response finished",
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
                    if let Ok(frames) = self.decoder.finish() {
                        self.frames.extend(frames);
                    }
                    while let Some(frame) = self.frames.pop_front() {
                        match self.translator.translate(&frame.event, &frame.data) {
                            Ok(events) => self.queue.extend(events.into_iter().map(Ok)),
                            Err(error) => self.queue.push_back(Err(error)),
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
}

#[async_trait]
impl Provider for TraeProvider {
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

    /// Local validation only: the credential must parse and its expiry must be
    /// in the future. Nothing is sent to the upstream.
    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        if credential.is_expired(self.now()) {
            return Ok(AuthStatus::Expired);
        }
        Ok(AuthStatus::Valid)
    }

    /// Refresh an expiring credential when an account id is available, then
    /// report the resulting state. Only explicit refresh rejections become
    /// `Invalid`/`Restricted`; transport failures stay errors.
    async fn auth_status_with_context(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        match self.credential_for(context, credential).await {
            Ok(credential) if credential.is_expired(self.now()) => Ok(AuthStatus::Expired),
            Ok(_) => Ok(AuthStatus::Valid),
            Err(error) => match error.kind {
                ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
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
        self.ensure_usable(&credential)?;
        let payload = self
            .http
            .post_json(&self.http.models_url(), &models_request_body(), &credential)
            .await?;
        parse_catalog(self.info.region, &payload)
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

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        self.ensure_usable(&credential)?;
        self.quota_with(&credential).await
    }

    async fn quota_with_context(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let current = self.credential_for(context, credential).await?;
        match self.quota_with(&current).await {
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                let retry = self.credential_for_with(context, credential, true).await?;
                self.quota_with(&retry).await
            }
            other => other,
        }
    }

    async fn chat_with_context(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let current = self.credential_for(context, credential).await?;
        match self.chat_with(current, request.clone()).await {
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                // One forced refresh, only after the previous attempt failed
                // before returning content. A second failure is final.
                let retry = self.credential_for_with(context, credential, true).await?;
                self.chat_with(retry, request).await
            }
            other => other,
        }
    }

    async fn chat_stream_with_context(
        &self,
        context: &crate::providers::ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let current = self.credential_for(context, credential).await?;
        match self
            .open_stream_with(current, request.clone(), uuid::Uuid::new_v4().to_string())
            .await
        {
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                let retry = self.credential_for_with(context, credential, true).await?;
                self.open_stream_with(retry, request, uuid::Uuid::new_v4().to_string())
                    .await
            }
            other => other,
        }
    }
}
