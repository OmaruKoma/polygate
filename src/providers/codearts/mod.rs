//! Huawei CodeArts (snap engine) provider adapter.
//!
//! Protocol sources are the three read-only reference implementations listed in
//! `docs/CODEARTS_SESSION.md`:
//!
//! * **chat** `POST {snap}/api/v2/chat/completions` — standard OpenAI chunks,
//!   forced `stream=true`, `chat_id`/`prompt_cache_key` for upstream affinity;
//! * **models** `GET {snap}/v1/model/builtin` (`Agent-Type: PromptCenter`);
//! * **quota** `GET {snap}/snap-manager/v1/statistics/plugin`;
//! * **check-in** `/v1/ops/*` (implemented in `crate::checkin::codearts`).
//!
//! Authentication is AK/SK `SDK-HMAC-SHA256` plus the STS `security_token`.
//! Expiring credentials are refreshed with the stored one-time refresh token
//! and DPoP key, then persisted through [`CredentialStore`] with a revision
//! compare-and-swap. Refresh is serialized per account so a one-time token is
//! never consumed twice.
//!
//! Verification state is `experimental` until a real account has been exercised
//! end to end. Nothing in this module claims that has happened.

pub(crate) mod auth;
mod client;
mod convert;
mod error;
pub mod login;
mod models;
mod quota;
pub(crate) mod signer;

#[cfg(test)]
mod tests;

pub use auth::{CodeArtsCredential, REFRESH_SKEW};
pub use client::{ClientHeaders, CodeArtsHttp, CHAT_PATH, MODEL_BUILTIN_PATH, QUOTA_PATH};
pub use convert::build_body;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use sha2::{Digest, Sha256};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, Provider, ProviderContext, ProviderInfo, StreamItem, UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use self::convert::{parse_sse_line, SseLine, StreamTranslator};
use self::models::parse_models;
use self::quota::parse_quota;

/// Wall-clock injection for tests (`None` means `Utc::now()`).
pub type ClockFn = Arc<dyn Fn() -> chrono::DateTime<Utc> + Send + Sync>;

#[derive(Clone)]
pub struct CodeArtsProvider {
    info: ProviderInfo,
    http: CodeArtsHttp,
    store: Option<Arc<dyn crate::providers::CredentialStore>>,
    refresh_locks: Arc<parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    clock: ClockFn,
}

impl std::fmt::Debug for CodeArtsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeArtsProvider")
            .field("id", &self.info.id)
            .field("base_url", &self.http.base_url())
            .field("can_refresh", &self.store.is_some())
            .finish_non_exhaustive()
    }
}

impl CodeArtsProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        base_url: impl Into<String>,
        sts_url: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let id = id.into();
        let http = CodeArtsHttp::new(base_url, sts_url)?;
        let info = ProviderInfo {
            id,
            kind: ProviderKind::Codearts,
            region,
            display_name: format!(
                "{} ({})",
                ProviderKind::Codearts.display_name(),
                region.as_str().to_uppercase()
            ),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Ok(Self {
            info,
            http,
            store: None,
            refresh_locks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            clock: Arc::new(Utc::now),
        })
    }

    pub fn with_credentials_store(
        mut self,
        store: Arc<dyn crate::providers::CredentialStore>,
    ) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_http(mut self, http: CodeArtsHttp) -> Self {
        self.http = http;
        self
    }

    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }

    pub fn http(&self) -> &CodeArtsHttp {
        &self.http
    }

    fn now(&self) -> chrono::DateTime<Utc> {
        (self.clock)()
    }

    /// Non-streamed chat: aggregate the same upstream SSE stream locally. The
    /// CodeArts endpoint has no non-streaming mode.
    pub async fn chat_for(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        let request_id = request.request_id.clone();
        let model = request.model.clone();
        let mut stream = self
            .open_stream(context, credential, request, response_id.clone())
            .await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return Err(error),
            }
        }
        Ok(convert::aggregate(
            &response_id,
            request_id,
            &model,
            &events,
        ))
    }

    pub async fn chat_stream_for(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        self.open_stream(context, credential, request, response_id)
            .await
    }

    async fn open_stream(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
        response_id: String,
    ) -> Result<ChatStream, GatewayError> {
        let credential = self.ensure_fresh(context, credential).await?;
        let chat_id = chat_id_for(context);
        let body = convert::build_body(&request, &chat_id)?;
        let response = self.http.open_chat(&body, &credential, self.now()).await?;

        let translator = StreamTranslator::new(
            response_id,
            request.request_id.clone(),
            request.model.clone(),
        );
        let state = StreamState {
            body: response,
            translator,
            queue: std::collections::VecDeque::new(),
            started: false,
            done: false,
        };
        let events = stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        });
        Ok(Box::pin(events))
    }

    /// Refresh an expiring credential once per account, persisting it with CAS.
    async fn ensure_fresh(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<CodeArtsCredential, GatewayError> {
        let parsed = CodeArtsCredential::parse(credential)?;
        if !parsed.expiring(self.now()) {
            return Ok(parsed);
        }
        if parsed.expired(self.now()) && !parsed.can_refresh() {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "CodeArts credential expired and has no refresh material",
            ));
        }
        if !parsed.can_refresh() {
            return Ok(parsed);
        }
        self.rotate(context, credential, false).await
    }

    /// Rotate the STS token. `forced` skips the "is it expiring" question: the
    /// upstream already rejected a token the credential believes is fresh.
    /// The refresh token is one-time, so the lock and the reload come first.
    async fn rotate(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        forced: bool,
    ) -> Result<CodeArtsCredential, GatewayError> {
        let (Some(store), Some(account_id)) = (&self.store, context.account_id.as_ref()) else {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "CodeArts credential expired; no credential store is configured",
            ));
        };
        let lock = {
            let mut locks = self.refresh_locks.lock();
            locks
                .entry(account_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock_owned().await;

        // Another request may have refreshed while this one waited.
        let (latest, revision) = match store.load_credential(account_id).await? {
            Some(value) => value,
            None => (credential.clone(), context.credential_revision.unwrap_or(0)),
        };
        let latest_parsed = CodeArtsCredential::parse(&latest)?;
        let rotated_elsewhere = revision != context.credential_revision.unwrap_or(0);
        if (!latest_parsed.expiring(self.now()) && !forced) || (forced && rotated_elsewhere) {
            return Ok(latest_parsed);
        }

        let refreshed = self
            .http
            .refresh(&latest_parsed, &latest, self.now())
            .await?;
        match store
            .update_credential(account_id, revision, &refreshed)
            .await
        {
            Ok(_) => CodeArtsCredential::parse(&refreshed),
            Err(_) => {
                // A concurrent writer won. Reload once; if it wrote a fresh
                // credential, use it. Never refresh twice: the refresh token is
                // one-time.
                let (stored, _) = store.load_credential(account_id).await?.ok_or_else(|| {
                    GatewayError::new(
                        ErrorKind::AuthExpired,
                        "CodeArts credential disappeared during refresh",
                    )
                })?;
                let stored = CodeArtsCredential::parse(&stored)?;
                if stored.expiring(self.now()) {
                    return Err(GatewayError::new(
                        ErrorKind::AuthExpired,
                        "CodeArts credential refresh was superseded and the stored credential is still expiring",
                    ));
                }
                Ok(stored)
            }
        }
    }

    /// Quota for one account. The `Provider::quota` method cannot know the
    /// account id, so callers that persist snapshots use this variant.
    pub async fn quota_for(
        &self,
        account_id: &str,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = CodeArtsCredential::parse(credential)?;
        let payload = self
            .http
            .get_json(
                client::STATISTICS_PLUGIN_PATH,
                &credential,
                Some("PromptCenter"),
                self.now(),
            )
            .await?;
        Ok(parse_quota(account_id, &payload, self.now()))
    }

    pub async fn auth_status_for(
        &self,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        let credential = CodeArtsCredential::parse(credential)?;
        self.probe_status(&credential).await
    }

    /// Probe with the account context so an expiring credential is refreshed
    /// once before the upstream check. Without the context (no account id) a
    /// refresh cannot be persisted, so the credential is only inspected.
    pub async fn auth_status_with_context_for(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        let fresh = match self.ensure_fresh(context, credential).await {
            Ok(fresh) => fresh,
            // A rejected refresh is an account-state answer, not a gateway
            // failure: the operator has to re-authorize. Transport errors stay
            // errors so the UI can report an upstream problem instead.
            Err(error) => return refresh_failure(error),
        };
        match self.probe_status(&fresh).await? {
            // The upstream rejected a token whose recorded expiry is still in
            // the future (revoked, or the clock differs). Rotate once; without
            // this a stale but "fresh-looking" credential would keep a provider
            // out of the catalogue until the operator re-authorizes.
            AuthStatus::RefreshRequired => match self.rotate(context, credential, true).await {
                Ok(rotated) => self.probe_status(&rotated).await,
                Err(error) => refresh_failure(error),
            },
            status => Ok(status),
        }
    }

    async fn probe_status(
        &self,
        credential: &CodeArtsCredential,
    ) -> Result<AuthStatus, GatewayError> {
        if credential.expired(self.now()) {
            return Ok(AuthStatus::Expired);
        }
        match self
            .http
            .get_json(
                client::MODEL_BUILTIN_PATH,
                credential,
                Some("PromptCenter"),
                self.now(),
            )
            .await
        {
            Ok(_) => Ok(AuthStatus::Valid),
            Err(error) => match error.kind {
                ErrorKind::AuthExpired => Ok(AuthStatus::RefreshRequired),
                ErrorKind::AuthInvalid => Ok(AuthStatus::Invalid),
                ErrorKind::ChannelRestricted => Ok(AuthStatus::Restricted),
                _ => Err(error),
            },
        }
    }
}

fn chat_id_for(context: &ProviderContext) -> String {
    if let Some(id) = context
        .upstream_session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
    if let Some(client_session) = context.client_session_id.as_deref() {
        // 32 hex like the upstream client. Deterministic so two requests without
        // a binding still land on the same upstream cache key.
        let digest = Sha256::new_with_prefix(client_session.as_bytes()).finalize();
        return hex::encode(digest)[..32].to_string();
    }
    uuid::Uuid::new_v4().simple().to_string()
}

struct StreamState {
    body: client::SseBody,
    translator: StreamTranslator,
    queue: std::collections::VecDeque<StreamItem>,
    started: bool,
    done: bool,
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
            match self.body.next_line().await {
                Ok(Some(line)) => match parse_sse_line(&line) {
                    Ok(SseLine::Ignore) => continue,
                    Ok(SseLine::Done) => {
                        self.done = true;
                        self.queue
                            .extend(self.translator.finish_events().into_iter().map(Ok));
                        return self.queue.pop_front();
                    }
                    Ok(SseLine::Payload(payload)) => match self.translator.translate(&payload) {
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
                    },
                    Err(error) => {
                        self.done = true;
                        return Some(Err(error));
                    }
                },
                Ok(None) => {
                    self.done = true;
                    self.queue
                        .extend(self.translator.finish_events().into_iter().map(Ok));
                    return self.queue.pop_front();
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[async_trait]
impl Provider for CodeArtsProvider {
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
            // CodeArts `chat_id` is prompt-cache affinity, not resumable context;
            // `docs/PROVIDER_MIGRATION.md` §2.1 keeps NativeSession false.
            native_session: false,
            agent_task: false,
            cancellation: false,
        }
    }

    async fn auth_status(&self, credential: &Credential) -> Result<AuthStatus, GatewayError> {
        self.auth_status_for(credential).await
    }

    async fn auth_status_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        self.auth_status_with_context_for(context, credential).await
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let credential = CodeArtsCredential::parse(credential)?;
        let payload = self
            .http
            .get_json(
                client::MODEL_BUILTIN_PATH,
                &credential,
                Some("PromptCenter"),
                self.now(),
            )
            .await?;
        Ok(parse_models(&payload))
    }

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        self.quota_for("", credential).await
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        self.chat_for(&ProviderContext::default(), credential, request)
            .await
    }

    async fn chat_stream(
        &self,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.chat_stream_for(&ProviderContext::default(), credential, request)
            .await
    }

    async fn chat_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: crate::protocol::ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        self.chat_for(context, credential, request).await
    }

    async fn chat_stream_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.chat_stream_for(context, credential, request).await
    }
}

/// Map a failed refresh onto an account status.
///
/// Credential problems are an account-state answer — the operator has to
/// re-authorize — so they must not surface as a gateway error. Everything else
/// (transport, timeout, unexpected 5xx) stays an error: pretending a network
/// failure is "expired" would send the operator through authorization for
/// nothing.
fn refresh_failure(error: GatewayError) -> Result<AuthStatus, GatewayError> {
    tracing::warn!(
        kind = ?error.kind,
        detail = %error.message,
        "CodeArts credential refresh failed"
    );
    match error.kind {
        ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
        ErrorKind::AuthInvalid | ErrorKind::AccountDisabled => Ok(AuthStatus::Invalid),
        ErrorKind::ChannelRestricted => Ok(AuthStatus::Restricted),
        _ => Err(error),
    }
}
