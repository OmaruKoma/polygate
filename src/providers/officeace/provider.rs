//! OfficeAce provider: identity, catalogue, quota and inference.
//!
//! The model gateway is OpenAI-shaped, so inference is a projection both ways
//! (see [`super::convert`] and [`super::events`]). There is no upstream session
//! id and no cancel endpoint, so `native_session`, `agent_task` and
//! `cancellation` stay `false`.
//!
//! Credentials hold two families: a permanent `Basic` app key for chat and a
//! temporary AK/SK for quota/check-in. Only the temporary half expires; it is
//! refreshed with a one-time refresh token plus the DPoP key that was bound to
//! it, then persisted with a compare-and-swap so two requests never consume the
//! same token.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{ChatRequest, ChatResponse};
use crate::providers::{
    Capabilities, ChatStream, Provider, ProviderContext, ProviderInfo, StreamItem, UpstreamModel,
};
use crate::quota::QuotaSnapshot;

use super::convert;
use super::credential::OfficeAceCredential;
use super::events::{parse_frame, Frame, SseDecoder, Translator};
use super::http::OfficeAceHttp;
use super::models::parse_catalog;
use super::region;
use super::subscription;

/// Refresh a temporary AK/SK this long before it expires.
pub const REFRESH_SKEW: chrono::Duration = chrono::Duration::seconds(600);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub type ClockFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct OfficeAceProvider {
    info: ProviderInfo,
    http: Arc<OfficeAceHttp>,
    store: Option<Arc<dyn crate::providers::CredentialStore>>,
    refresh_locks: Arc<parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    clock: ClockFn,
    stream_idle: Duration,
}

impl std::fmt::Debug for OfficeAceProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfficeAceProvider")
            .field("id", &self.info.id)
            .field("cloud", &self.http.cloud())
            .field("can_refresh", &self.store.is_some())
            .finish_non_exhaustive()
    }
}

impl OfficeAceProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        Self::build(id, region, verification, OfficeAceHttp::new(client))
    }

    /// Tests: point control plane and STS at one local mock server.
    pub fn with_endpoints(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        cloud: impl Into<String>,
        sts: impl Into<String>,
    ) -> Self {
        Self::build(
            id,
            region,
            verification,
            OfficeAceHttp::with_endpoints(client, cloud, sts),
        )
    }

    fn build(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        http: OfficeAceHttp,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Officeace,
            region,
            display_name: ProviderKind::Officeace.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            http: Arc::new(http),
            store: None,
            refresh_locks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            clock: Arc::new(Utc::now),
            stream_idle: STREAM_IDLE_TIMEOUT,
        }
    }

    pub fn with_credentials_store(
        mut self,
        store: Arc<dyn crate::providers::CredentialStore>,
    ) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_http(mut self, http: OfficeAceHttp) -> Self {
        self.http = Arc::new(http);
        self
    }

    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }

    pub fn http(&self) -> &OfficeAceHttp {
        &self.http
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    fn parse(&self, credential: &Credential) -> Result<OfficeAceCredential, GatewayError> {
        OfficeAceCredential::from_credential(credential)
    }

    /// Refresh an expiring temporary AK/SK once per account, persisting it
    /// with a revision compare-and-swap.
    async fn ensure_fresh(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<OfficeAceCredential, GatewayError> {
        let parsed = self.parse(credential)?;
        if !parsed.expiring(self.now(), REFRESH_SKEW) {
            return Ok(parsed);
        }
        if !parsed.can_refresh() {
            // Chat still works with the permanent app key; only quota and
            // check-in fail until the operator re-authorizes.
            return Ok(parsed);
        }
        self.rotate(context, credential, false).await
    }

    async fn rotate(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        forced: bool,
    ) -> Result<OfficeAceCredential, GatewayError> {
        let (Some(store), Some(account_id)) = (&self.store, context.account_id.as_ref()) else {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "OfficeAce temporary credential expired; no credential store is configured",
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

        let (latest, revision) = match store.load_credential(account_id).await? {
            Some(value) => value,
            None => (credential.clone(), context.credential_revision.unwrap_or(0)),
        };
        let latest_parsed = self.parse(&latest)?;
        let rotated_elsewhere = revision != context.credential_revision.unwrap_or(0);
        if (!latest_parsed.expiring(self.now(), REFRESH_SKEW) && !forced)
            || (forced && rotated_elsewhere)
        {
            return Ok(latest_parsed);
        }

        let refreshed = self.refresh(&latest_parsed, &latest).await?;
        match store
            .update_credential(account_id, revision, &refreshed)
            .await
        {
            Ok(_) => self.parse(&refreshed),
            Err(_) => {
                // Another writer won the race. Never refresh twice: the token
                // is one-time.
                let (stored, _) = store.load_credential(account_id).await?.ok_or_else(|| {
                    GatewayError::new(
                        ErrorKind::AuthExpired,
                        "OfficeAce credential disappeared during refresh",
                    )
                })?;
                let stored = self.parse(&stored)?;
                if stored.expiring(self.now(), REFRESH_SKEW) {
                    return Err(GatewayError::new(
                        ErrorKind::AuthExpired,
                        "OfficeAce credential refresh was superseded and the stored credential is still expiring",
                    ));
                }
                Ok(stored)
            }
        }
    }

    async fn refresh(
        &self,
        parsed: &OfficeAceCredential,
        original: &Credential,
    ) -> Result<Credential, GatewayError> {
        let refresh_token = parsed.refresh_token.as_ref().ok_or_else(|| {
            GatewayError::new(
                ErrorKind::AuthExpired,
                "OfficeAce credential has no refresh token; re-login is required",
            )
        })?;
        let dpop = parsed.dpop.as_ref().ok_or_else(|| {
            GatewayError::new(
                ErrorKind::AuthExpired,
                "OfficeAce credential has no DPoP key; re-login is required",
            )
        })?;
        let token_url = format!("{}{}", self.http.sts(), region::EP_OAUTH_TOKENS);
        let proof = dpop.proof(&token_url, self.now())?;
        let form = vec![
            ("client_id", parsed.client_id.clone()),
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.to_string()),
        ];
        let payload = self.http.token_request(&form, proof).await?;
        let credentials = payload.get("credentials").ok_or_else(|| {
            GatewayError::upstream_server("OfficeAce token response is missing credentials")
        })?;
        let required = |key: &str| -> Result<&str, GatewayError> {
            credentials
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    GatewayError::upstream_server(format!(
                        "OfficeAce token response is missing {key}"
                    ))
                })
        };
        let mut updated = original.to_json().clone();
        if let Some(object) = updated.as_object_mut() {
            object.insert("access_key_id".into(), json!(required("access_key_id")?));
            object.insert(
                "secret_access_key".into(),
                json!(required("secret_access_key")?),
            );
            object.insert("security_token".into(), json!(required("security_token")?));
            if let Some(expiration) = credentials.get("expiration").and_then(Value::as_str) {
                object.insert("expiration".into(), json!(expiration));
            }
            if let Some(project_id) = credentials.get("project_id").and_then(Value::as_str) {
                if !project_id.is_empty() {
                    object.insert("project_id".into(), json!(project_id));
                }
            }
            if let Some(rotated) = payload.get("refresh_token").and_then(Value::as_str) {
                if !rotated.is_empty() {
                    object.insert("refresh_token".into(), json!(rotated));
                }
            }
        }
        Ok(Credential::from_value(updated))
    }

    async fn probe(&self, credential: &OfficeAceCredential) -> Result<AuthStatus, GatewayError> {
        match self.http.get_models(credential).await {
            Ok(_) => Ok(AuthStatus::Valid),
            // The Basic app key never expires; a 401 means it was revoked.
            Err(error) if error.kind == ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
            Err(error) if error.kind == ErrorKind::AuthInvalid => Ok(AuthStatus::Invalid),
            Err(error) if error.kind == ErrorKind::RateLimited => Ok(AuthStatus::Restricted),
            Err(error) => Err(error),
        }
    }

    async fn open_stream_with(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let credential = self.ensure_fresh(context, credential).await?;
        let body = convert::build_body(&request)?;
        let response = self.http.open_stream(&credential, &body).await?;
        let state = StreamState {
            body: response,
            decoder: SseDecoder::new(),
            translator: Translator::new(request.model.clone()),
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
impl Provider for OfficeAceProvider {
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
        let credential = self.parse(credential)?;
        self.probe(&credential).await
    }

    async fn auth_status_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        let fresh = match self.ensure_fresh(context, credential).await {
            Ok(fresh) => fresh,
            Err(error) => return refresh_failure(error),
        };
        match self.probe(&fresh).await? {
            AuthStatus::RefreshRequired => match self.rotate(context, credential, true).await {
                Ok(rotated) => self.probe(&rotated).await,
                Err(error) => refresh_failure(error),
            },
            status => Ok(status),
        }
    }

    async fn list_models(
        &self,
        credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        let credential = self.parse(credential)?;
        let value = self.http.get_models(&credential).await?;
        parse_catalog(&value)
    }

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.parse(credential)?;
        self.quota_snapshot("", &credential).await
    }

    async fn quota_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        let credential = self.ensure_fresh(context, credential).await?;
        let account_id = context.account_id.clone().unwrap_or_default();
        self.quota_snapshot(&account_id, &credential).await
    }

    async fn chat(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        self.chat_with_context(&ProviderContext::default(), credential, request)
            .await
    }

    async fn chat_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let response_id = uuid::Uuid::new_v4().to_string();
        let request_id = request.request_id.clone();
        let model = request.model.clone();
        let mut stream = self.open_stream_with(context, credential, request).await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return Err(error),
            }
        }
        Ok(convert::aggregate(response_id, request_id, model, &events))
    }

    async fn chat_stream(
        &self,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.open_stream_with(&ProviderContext::default(), credential, request)
            .await
    }

    async fn chat_stream_with_context(
        &self,
        context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.open_stream_with(context, credential, request).await
    }
}

impl OfficeAceProvider {
    async fn quota_snapshot(
        &self,
        account_id: &str,
        credential: &OfficeAceCredential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        if !credential.has_control_plane() {
            // "Cannot read" is not "zero": keep the numbers absent.
            return Ok(QuotaSnapshot {
                account_id: account_id.to_string(),
                group_id: None,
                model: None,
                unit: crate::quota::QuotaUnit::Credits,
                total: None,
                remaining: None,
                status: crate::quota::QuotaStatus::Unknown,
                error: Some(
                    "the account has no temporary AK/SK, so points cannot be read; re-authorize it"
                        .into(),
                ),
                refreshed_at: self.now(),
                raw: None,
            });
        }
        match self.http.get_subscription(credential, self.now()).await {
            Ok(value) => subscription::parse_snapshot(account_id, &value, self.now()),
            Err(error) if matches!(error.kind, ErrorKind::AuthExpired | ErrorKind::AuthInvalid) => {
                Err(error)
            }
            Err(error) => Ok(QuotaSnapshot {
                account_id: account_id.to_string(),
                group_id: None,
                model: None,
                unit: crate::quota::QuotaUnit::Credits,
                total: None,
                remaining: None,
                status: crate::quota::QuotaStatus::Error,
                error: Some(error.message),
                refreshed_at: self.now(),
                raw: None,
            }),
        }
    }
}

fn refresh_failure(error: GatewayError) -> Result<AuthStatus, GatewayError> {
    tracing::warn!(kind = ?error.kind, detail = %error.message, "OfficeAce credential refresh failed");
    match error.kind {
        ErrorKind::AuthExpired => Ok(AuthStatus::Expired),
        ErrorKind::AuthInvalid | ErrorKind::AccountDisabled => Ok(AuthStatus::Invalid),
        ErrorKind::ChannelRestricted => Ok(AuthStatus::Restricted),
        _ => Err(error),
    }
}

struct StreamState {
    body: reqwest::Response,
    decoder: SseDecoder,
    translator: Translator,
    frames: VecDeque<String>,
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
                        "OfficeAce stream went idle",
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

    fn provider() -> OfficeAceProvider {
        OfficeAceProvider::new(
            "officeace-cn",
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
        assert_eq!(info.kind, ProviderKind::Officeace);
        assert_eq!(info.region, Region::Cn);
    }
}
