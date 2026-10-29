//! WorkBuddy provider: identity, catalogue and inference.
//!
//! Chat is OpenAI-shaped over SSE; the upstream has no non-streaming mode, so
//! [`Provider::chat`] aggregates the same stream locally.
//!
//! There is no upstream session id and no cancellation endpoint, so
//! `native_session`, `agent_task` and `cancellation` stay `false`. Quota and
//! check-in are M14-C work; until then `quota` reports `Unsupported` instead of
//! a fabricated number.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use parking_lot::Mutex;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, CredentialStore, Provider, ProviderContext, ProviderInfo, StreamItem,
    UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use super::convert;
use super::credential::WorkbuddyCredential;
use super::error::classify_transport;
use super::events::{SseDecoder, StreamTranslator};
use super::http::WorkbuddyHttp;
use super::models::parse_catalog;
use super::quota::{parse_quota, quota_request_body};
use super::region;

/// A stream that produces no bytes for this long is treated as interrupted;
/// otherwise a stalled upstream would hold the account lease forever.
const STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// Refresh this long before the access token expires.
const REFRESH_SKEW_SECONDS: i64 = 300;
/// Cross-process lease lifetime for one refresh call.
const REFRESH_LEASE_SECONDS: i64 = 60;
/// While another process holds the refresh lease, poll for its result.
const REFRESH_WAIT_STEP: std::time::Duration = std::time::Duration::from_millis(250);
const REFRESH_WAIT_STEPS: usize = 20;

pub struct WorkbuddyProvider {
    info: ProviderInfo,
    http: Arc<WorkbuddyHttp>,
    stream_idle: std::time::Duration,
    store: Option<Arc<dyn CredentialStore>>,
    refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Identifies this process in the shared refresh lease.
    refresh_owner: String,
    refresh_wait_steps: usize,
    refresh_wait_step: std::time::Duration,
}

impl std::fmt::Debug for WorkbuddyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkbuddyProvider")
            .field("id", &self.info.id)
            .field("region", &self.info.region)
            .field("base", &self.http.base())
            .finish_non_exhaustive()
    }
}

impl WorkbuddyProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Workbuddy,
            region,
            display_name: format!(
                "{} ({})",
                ProviderKind::Workbuddy.display_name(),
                region.as_str().to_uppercase()
            ),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(WorkbuddyHttp::new(client, region)),
            stream_idle: STREAM_IDLE_TIMEOUT,
            store: None,
            refresh_locks: Mutex::new(HashMap::new()),
            refresh_owner: uuid::Uuid::new_v4().to_string(),
            refresh_wait_steps: REFRESH_WAIT_STEPS,
            refresh_wait_step: REFRESH_WAIT_STEP,
        }
    }

    pub fn with_credentials_store(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Send the official desktop client's identity headers instead of the
    /// gateway's own. See [`crate::providers::profile`].
    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        Arc::make_mut(&mut self.http).set_identity(identity);
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
        provider.http = Arc::new(WorkbuddyHttp::with_base(client, region, base));
        provider
    }

    #[cfg(test)]
    pub fn with_timeouts(
        mut self,
        request: std::time::Duration,
        idle: std::time::Duration,
    ) -> Self {
        let client = self.http.client().clone();
        let identity = self.http.identity();
        self.http = Arc::new(
            WorkbuddyHttp::with_base(client, self.info.region, self.http.base())
                .with_request_timeout(request)
                .with_identity(identity),
        );
        self.stream_idle = idle;
        self
    }

    /// Tests only: shorten the wait for another process's refresh.
    #[cfg(test)]
    pub fn with_refresh_wait(mut self, steps: usize, step: std::time::Duration) -> Self {
        self.refresh_wait_steps = steps;
        self.refresh_wait_step = step;
        self
    }

    fn credential(&self, credential: &Credential) -> Result<WorkbuddyCredential, GatewayError> {
        WorkbuddyCredential::from_credential(credential, self.info.region)
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
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<WorkbuddyCredential, GatewayError> {
        self.credential_for_with(context, credential, false).await
    }

    /// `force` refreshes even when the access token looks valid; it is used for
    /// a single retry after the upstream rejected an otherwise unexpired token.
    async fn credential_for_with(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        force: bool,
    ) -> Result<WorkbuddyCredential, GatewayError> {
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
                    "WorkBuddy credential expired; no credential store is available to persist a refresh",
                ));
            }
            return Ok(parsed);
        };
        if !parsed.can_refresh(now) {
            if expired {
                return Err(GatewayError::new(
                    ErrorKind::AuthExpired,
                    "WorkBuddy credential expired and has no usable refresh token",
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
        if !latest.can_refresh(now) {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "WorkBuddy credential expired and has no usable refresh token",
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
                // that; never refresh twice, the refresh token is rotated.
                if let Some((stored, _)) = store.load_credential(account_id).await? {
                    let stored = self.credential(&stored)?;
                    if !stored.needs_refresh(self.now(), REFRESH_SKEW_SECONDS) {
                        return Ok(stored);
                    }
                }
                if update_error.kind == ErrorKind::Storage {
                    // The rotated token is now only in memory and is about to be
                    // dropped; the caller must not see a successful refresh.
                    return Err(update_error);
                }
                Err(GatewayError::new(
                    ErrorKind::AuthExpired,
                    "WorkBuddy credential refresh was not persisted; authorize the account again",
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
    ) -> Result<WorkbuddyCredential, GatewayError> {
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
            "another PolyGate process is refreshing this WorkBuddy account; retry shortly",
        ))
    }

    async fn quota_with(
        &self,
        credential: &WorkbuddyCredential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let now = self.now();
        let payload = self
            .http
            .post_billing_json(
                region::USER_RESOURCE_PATH,
                &quota_request_body(now),
                credential,
            )
            .await?;
        parse_quota(&payload, now)
    }

    pub async fn chat_for(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let credential = self.credential(credential)?;
        self.chat_with(credential, request).await
    }

    async fn chat_with(
        &self,
        credential: WorkbuddyCredential,
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
        let response_id = uuid::Uuid::new_v4().to_string();
        self.open_stream_with(credential, request, response_id)
            .await
    }

    async fn open_stream_with(
        &self,
        credential: WorkbuddyCredential,
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
    frames: VecDeque<String>,
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
                match self.translator.translate(&frame) {
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
                        "WorkBuddy stream went idle before the response finished",
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
                        match self.translator.translate(&frame) {
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
impl Provider for WorkbuddyProvider {
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
    /// in the future. `Invalid`/`Restricted` are never inferred without an
    /// upstream probe.
    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        let credential = self.credential(credential)?;
        if credential.is_expired(Utc::now().timestamp()) {
            return Ok(AuthStatus::Expired);
        }
        Ok(AuthStatus::Valid)
    }

    /// Refresh an expiring credential when an account id is available, then
    /// report the resulting state. Only explicit refresh rejections become
    /// `Invalid`/`Restricted`; transport failures stay errors.
    async fn auth_status_with_context(
        &self,
        context: &ProviderContext,
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

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential(credential)?;
        self.quota_with(&credential).await
    }

    async fn quota_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.credential_for(context, credential).await?;
        self.quota_with(&credential).await
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let credential = self.credential(credential)?;
        let path = region::realm(self.info.region).models_path(&credential.enterprise_id)?;
        let payload = self.http.get_json(&path, &credential).await?;
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

    async fn chat_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let current = self.credential_for(context, credential).await?;
        match self.chat_with(current, request.clone()).await {
            Err(error) if error.kind == ErrorKind::AuthExpired => {
                // One forced refresh, only before any content was returned. A
                // second failure is final; there is no retry loop.
                let retry = self.credential_for_with(context, credential, true).await?;
                self.chat_with(retry, request).await
            }
            other => other,
        }
    }

    async fn chat_stream_with_context(
        &self,
        context: &ProviderContext,
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
