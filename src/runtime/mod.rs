//! Gateway runtime: configuration → security → storage → registry → HTTP.
//!
//! [`Gateway::bind`] performs every fallible startup step (except serving) so a
//! misconfigured process fails before accepting connections:
//!
//! 1. create the data directory,
//! 2. require at least one gateway API key,
//! 3. validate the credential master key (never logged),
//! 4. open SQLite and apply migrations,
//! 5. build the provider registry from explicitly enabled providers,
//! 6. discover models, build API state and the Axum router,
//! 7. bind the TCP listener.
//!
//! [`Gateway::serve`] runs the HTTP server until the shutdown future resolves,
//! then closes the database pool.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use chrono::Utc;
use serde::Deserialize;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::account_service::{AccountCredentialStore, AccountService, AuthorizationContext};
use crate::admin::{self, AdminState};
use crate::api::{self, ApiKeyAuth, ApiState, ModelCatalog};
use crate::checkin::{
    AuthRefreshHook, AuthRefreshRequest, CheckinExecutor, CheckinScheduler, CheckinService,
    CodeArtsCheckinExecutor, MockCheckinExecutor, OfficeAceCheckinExecutor, QoderCheckinExecutor,
    QuotaRefreshHook, QuotaRefreshRequest, RoutingCheckinExecutor, TraeCheckinExecutor,
    WorkbuddyCheckinExecutor,
};
use crate::config::{Config, ProviderConfig, ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::providers::profile::Identity;
use crate::providers::qoder::provider::QoderProvider;
use crate::providers::{
    ClientHeaders, CodeArtsHttp, CodeArtsProvider, LoomyProvider, MockConfig, MockProvider,
    MockReply, MockToolCall, OfficeAceProvider, ProviderKey, ProviderRegistry, TraeProvider,
    UpstreamModel, WorkbuddyProvider,
};
use crate::quota::QuotaSnapshot;
use crate::security::SecretBox;
use crate::session_service::{LimitPolicy, SessionBindingService, SessionRouter};
use crate::storage::Storage;

#[cfg(test)]
mod tests;

pub struct Gateway {
    config: Config,
    app: Router,
    registry: Arc<ProviderRegistry>,
    storage: Arc<Storage>,
    accounts: AccountService,
    sessions: SessionBindingService,
    checkin: CheckinService,
    checkin_scheduler: CheckinScheduler,
    listener: TcpListener,
    addr: SocketAddr,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("address", &self.addr)
            .field("providers", &self.registry.len())
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// Run every startup step and bind the listener. No request is served until
    /// [`Gateway::serve`] is called.
    pub async fn bind(config: Config) -> Result<Self, GatewayError> {
        config.prepare_dirs()?;

        let keys = config.gateway_keys();
        if keys.is_empty() {
            return Err(GatewayError::config(
                "no gateway API key configured; set POLYGATE_API_KEY or auth.api_keys (minimum 16 characters)",
            ));
        }

        // Fail on an unusable master key now instead of on the first credential
        // write. The key value never reaches a log line or error message.
        let secret_box = Arc::new(SecretBox::from_env_or_file(&config.data_dir)?);

        let storage = Arc::new(
            Storage::open_url(&config.database.url, config.database.max_connections).await?,
        );
        let accounts = AccountService::new((*storage).clone(), Arc::clone(&secret_box));
        // M13: CodeArts-derived session limits, applied per downstream session.
        let sessions = SessionBindingService::new(
            (*storage).clone(),
            accounts.clone(),
            LimitPolicy::codearts(),
        )?;
        let registry = Arc::new(build_registry(&config, &accounts)?);
        // Rotation/weights/quota/cooldown live in the scheduler; the session
        // router keeps stickiness and per-account concurrency.
        let scheduler = std::sync::Arc::new(crate::scheduler::Scheduler::new(
            config.scheduler.clone(),
            Some((*storage).clone()),
        ));
        let router =
            SessionRouter::with_scheduler(sessions.clone(), accounts.clone(), scheduler.clone());
        let model_routes = std::sync::Arc::new(crate::model_service::ModelRouteService::new(
            (*storage).clone(),
        ));
        // Mock + real check-in executors. CodeArts is registered only when its
        // provider is enabled; mock accounts remain mock-only.
        let executor = build_checkin_executor(&config)?;
        let checkin = CheckinService::new(
            (*storage).clone(),
            accounts.clone(),
            executor,
            Arc::new(LiveQuotaRefresh::new(
                accounts.clone(),
                Arc::clone(&registry),
                (*storage).clone(),
            )),
        )
        .with_auth_refresh(Arc::new(LiveAuthRefresh::new(
            accounts.clone(),
            Arc::clone(&registry),
        )));
        let checkin_scheduler = CheckinScheduler::new(checkin.clone());
        // Discovery uses the first available account per provider. Providers
        // without a usable account contribute no models (never a fake list).
        let catalog = Arc::new(ModelCatalog::discover_with_accounts(&registry, &accounts).await);
        let state = ApiState::new(
            Arc::clone(&registry),
            (*catalog).clone(),
            ApiKeyAuth::new(keys)?,
        )
        .with_router(router)
        .with_model_routes(model_routes, (*storage).clone());

        let bind_addr = config.server.socket_addr()?;
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| GatewayError::config(format!("cannot bind {bind_addr}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| GatewayError::config(format!("cannot read listener address: {e}")))?;

        let mut app = api::router(state);
        if config.admin.enabled {
            let mut admin_state = AdminState::new(
                &config,
                addr.port(),
                (*storage).clone(),
                accounts.clone(),
                Arc::clone(&registry),
                catalog,
                checkin.clone(),
                secret_box,
            );
            admin_state.scheduler = scheduler.clone();
            app = app.merge(admin::router(admin_state));
        }
        info!(address = %addr, "gateway listening");

        Ok(Self {
            config,
            app,
            registry,
            storage,
            accounts,
            sessions,
            checkin,
            checkin_scheduler,
            listener,
            addr,
        })
    }

    /// Address actually bound (port 0 resolves to the real port here).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn registry(&self) -> &Arc<ProviderRegistry> {
        &self.registry
    }

    /// Shared storage handle for diagnostics and tests.
    pub fn storage(&self) -> Arc<Storage> {
        Arc::clone(&self.storage)
    }

    /// Account/credential lifecycle service for this gateway instance.
    pub fn accounts(&self) -> &AccountService {
        &self.accounts
    }

    /// Session binding service (M13): stickiness, CAS updates and bounded
    /// account/session concurrency. HTTP wiring is intentionally absent.
    pub fn sessions(&self) -> &SessionBindingService {
        &self.sessions
    }

    /// Check-in service for this gateway instance (manual runs, schedules,
    /// recovery). HTTP/admin wiring is intentionally absent in M12.
    pub fn checkin(&self) -> &CheckinService {
        &self.checkin
    }

    /// Serve until `shutdown` resolves, then close the database pool.
    ///
    /// The check-in scheduler runs in a sibling task and is stopped after the
    /// HTTP server drains, so an in-flight account completes before the pool is
    /// closed.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), GatewayError> {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let retention_task = self.spawn_log_retention(stop_rx.clone());
        let scheduler = self.checkin_scheduler.clone();
        let scheduler_task = tokio::spawn(async move {
            let mut stop_rx = stop_rx;
            let stop = async move {
                let _ = stop_rx.wait_for(|stop| *stop).await;
            };
            scheduler.run(stop).await;
        });

        let result = axum::serve(
            self.listener,
            self.app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await;
        let _ = stop_tx.send(true);
        if let Err(error) = scheduler_task.await {
            tracing::warn!(error = %error, "check-in scheduler task failed");
        }
        if let Some(task) = retention_task {
            if let Err(error) = task.await {
                warn!(error = %error, "request-log retention task failed");
            }
        }
        info!("http server stopped; closing database pool");
        self.storage.close().await;
        result.map_err(|e| GatewayError::internal(format!("http server failed: {e}")))
    }

    /// Purge request logs older than `metrics.log_retention_days` at startup and
    /// every six hours. Disabled when the option is unset or request logging is
    /// off; a zero value is rejected during config validation.
    fn spawn_log_retention(
        &self,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let days = self.config.metrics.log_retention_days?;
        if !self.config.metrics.request_logs {
            return None;
        }
        let storage = Arc::clone(&self.storage);
        Some(tokio::spawn(async move {
            loop {
                let before = Utc::now() - chrono::Duration::days(i64::from(days));
                match storage.request_logs().purge_before(before).await {
                    Ok(deleted) if deleted > 0 => {
                        info!(deleted, days, "purged request logs past retention");
                    }
                    Ok(_) => {}
                    Err(error) => warn!(error = %error, "request-log retention purge failed"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(6 * 3600)) => {}
                    _ = stop.wait_for(|stop| *stop) => break,
                }
            }
        }))
    }
}

fn build_registry(
    config: &Config,
    accounts: &AccountService,
) -> Result<ProviderRegistry, GatewayError> {
    let registry = ProviderRegistry::new();
    for provider in &config.providers {
        if !provider.enabled {
            info!(provider = %provider.provider_id(), "provider disabled; not registered");
            continue;
        }
        match provider.kind {
            ProviderKind::Mock => {
                registry.register(Arc::new(MockProvider::new(mock_config(provider)?)))?;
            }
            ProviderKind::Codearts => {
                let (base_url, sts_url) = codearts_urls(provider);
                let http = CodeArtsHttp::new(base_url, sts_url)?.with_headers(ClientHeaders {
                    language: option(provider, "language").unwrap_or_else(|| "zh-cn".into()),
                    app_id: option(provider, "app_id").unwrap_or_else(|| "CodeAgent3.0".into()),
                    client_version: option(provider, "client_version")
                        .unwrap_or_else(|| "26.7.2".into()),
                });
                let codearts = CodeArtsProvider::new(
                    provider.provider_id(),
                    provider.region,
                    provider.verification(),
                    http.base_url().to_string(),
                    http.sts_url().to_string(),
                )?
                .with_http(http)
                .with_credentials_store(Arc::new(AccountCredentialStore::new(
                    accounts.credentials().clone(),
                )));
                registry.register(Arc::new(codearts))?;
            }
            ProviderKind::Qoder => {
                let qoder = QoderProvider::new(
                    provider.provider_id(),
                    provider.region,
                    provider.verification(),
                    reqwest::Client::new(),
                )
                .with_identity(identity(provider)?);
                registry.register(Arc::new(qoder))?;
            }
            ProviderKind::Loomy => {
                // One deployment only; a session issued for it is never valid
                // anywhere else, so an `intl` entry is a configuration error.
                if !crate::providers::loomy::region::supported(provider.region) {
                    return Err(GatewayError::config(
                        "provider loomy has no international deployment; use region = \"cn\"",
                    ));
                }
                let client = reqwest::Client::new();
                let loomy = match provider.base_url.as_deref() {
                    Some(base) => LoomyProvider::with_base(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                        base,
                    ),
                    None => LoomyProvider::new(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                    ),
                };
                registry.register(Arc::new(loomy))?;
            }
            ProviderKind::Officeace => {
                // One deployment (`cn-southwest-2`); an `intl` entry would need
                // endpoints that do not exist, so it is a configuration error.
                if !crate::providers::officeace::region::supported(provider.region) {
                    return Err(GatewayError::config(
                        "provider officeace has no international deployment; use region = \"cn\"",
                    ));
                }
                let cloud = option(provider, "cloud_url")
                    .unwrap_or_else(|| crate::providers::officeace::region::CLOUD_HOST.into());
                let sts = option(provider, "sts_url")
                    .unwrap_or_else(|| crate::providers::officeace::region::STS_HOST.into());
                let officeace = OfficeAceProvider::with_endpoints(
                    provider.provider_id(),
                    provider.region,
                    provider.verification(),
                    reqwest::Client::new(),
                    cloud,
                    sts,
                )
                .with_credentials_store(Arc::new(AccountCredentialStore::new(
                    accounts.credentials().clone(),
                )));
                registry.register(Arc::new(officeace))?;
            }
            ProviderKind::Workbuddy => {
                let client = reqwest::Client::new();
                let workbuddy = match provider.base_url.as_deref() {
                    Some(base) => WorkbuddyProvider::with_base(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                        base,
                    ),
                    None => WorkbuddyProvider::new(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                    ),
                }
                .with_credentials_store(Arc::new(AccountCredentialStore::new(
                    accounts.credentials().clone(),
                )))
                .with_identity(identity(provider)?);
                registry.register(Arc::new(workbuddy))?;
            }
            ProviderKind::Trae => {
                let client = reqwest::Client::new();
                let trae = match provider.base_url.as_deref() {
                    Some(base) => TraeProvider::with_base(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                        base,
                    ),
                    None => TraeProvider::new(
                        provider.provider_id(),
                        provider.region,
                        provider.verification(),
                        client,
                    ),
                }
                .with_credentials_store(Arc::new(AccountCredentialStore::new(
                    accounts.credentials().clone(),
                )))
                .with_ug_device_id(option(provider, "checkin.device_id"))
                .with_identity(identity(provider)?);
                registry.register(Arc::new(trae))?;
            }
        }
    }
    if registry.is_empty() {
        return Err(GatewayError::config(
            "no enabled providers configured; enable a provider or fix the provider entries",
        ));
    }
    Ok(registry)
}

fn build_checkin_executor(config: &Config) -> Result<Arc<dyn CheckinExecutor>, GatewayError> {
    let mock = Arc::new(MockCheckinExecutor::support_only([ProviderKind::Mock]));
    let mut routing = RoutingCheckinExecutor::new()
        .with(ProviderKind::Mock, Region::Cn, mock.clone())
        .with(ProviderKind::Mock, Region::Intl, mock);
    for provider in &config.providers {
        if !provider.enabled {
            continue;
        }
        match provider.kind {
            ProviderKind::Codearts => {
                let (base_url, sts_url) = codearts_urls(provider);
                routing = routing.with(
                    ProviderKind::Codearts,
                    provider.region,
                    Arc::new(CodeArtsCheckinExecutor::new(base_url, sts_url)?),
                );
            }
            ProviderKind::Officeace => {
                // Claims change account state, so they stay opt-in.
                let claim_enabled = option(provider, "checkin.claim_enabled")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"));
                let cloud = option(provider, "cloud_url")
                    .unwrap_or_else(|| crate::providers::officeace::region::CLOUD_HOST.into());
                let sts = option(provider, "sts_url")
                    .unwrap_or_else(|| crate::providers::officeace::region::STS_HOST.into());
                routing = routing.with(
                    ProviderKind::Officeace,
                    provider.region,
                    Arc::new(OfficeAceCheckinExecutor::new(
                        cloud,
                        sts,
                        claim_enabled,
                        reqwest::Client::new(),
                    )),
                );
            }
            ProviderKind::Qoder => {
                // Reward claims change account state, so they stay opt-in even
                // though the status query itself is read-only.
                let claim_enabled = option(provider, "checkin.claim_enabled")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"));
                routing = routing.with(
                    ProviderKind::Qoder,
                    provider.region,
                    Arc::new(
                        QoderCheckinExecutor::new(
                            provider.region,
                            claim_enabled,
                            reqwest::Client::new(),
                        )
                        .with_identity(identity(provider)?),
                    ),
                );
            }
            ProviderKind::Workbuddy => {
                // CN only in practice; the executor itself reports `Unsupported`
                // for Intl without sending a request. Claims stay opt-in.
                let claim_enabled = option(provider, "checkin.claim_enabled")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"));
                routing = routing.with(
                    ProviderKind::Workbuddy,
                    provider.region,
                    Arc::new(
                        WorkbuddyCheckinExecutor::new(
                            provider.region,
                            claim_enabled,
                            reqwest::Client::new(),
                        )
                        .with_identity(identity(provider)?),
                    ),
                );
            }
            ProviderKind::Trae => {
                // CN only. Claims stay opt-in, and a numeric device id must be
                // supplied by the operator; the executor never generates one.
                let claim_enabled = option(provider, "checkin.claim_enabled")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"));
                routing = routing.with(
                    ProviderKind::Trae,
                    provider.region,
                    Arc::new(
                        TraeCheckinExecutor::new(
                            provider.region,
                            claim_enabled,
                            option(provider, "checkin.device_id"),
                            reqwest::Client::new(),
                        )
                        .with_identity(identity(provider)?),
                    ),
                );
            }
            _ => {}
        }
    }
    Ok(Arc::new(routing))
}

/// `client.identity = impersonate` makes the adapter present the official
/// client's header family. Unset keeps the honest headers.
fn identity(provider: &ProviderConfig) -> Result<Identity, GatewayError> {
    let Some(raw) = option(provider, "client.identity") else {
        return Ok(Identity::default());
    };
    raw.parse().map_err(|_| {
        GatewayError::config(format!(
            "provider `{}` has an unknown `client.identity` value `{raw}` (use honest or impersonate)",
            provider.provider_id()
        ))
    })
}

fn option(provider: &ProviderConfig, key: &str) -> Option<String> {
    provider
        .options
        .get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn codearts_urls(provider: &ProviderConfig) -> (String, String) {
    let base_url = provider.base_url.clone().unwrap_or_else(|| {
        match provider.region {
            Region::Cn => "https://snap-access.cn-north-4.myhuaweicloud.com",
            Region::Intl => "https://snap-access.ap-southeast-1.myhuaweicloud.com",
        }
        .to_string()
    });
    // Both commercial profiles use the CN STS endpoint (local codearts2api
    // `app/config.py`, Coding2API `events.py`).
    let sts_url = option(provider, "sts_url")
        .unwrap_or_else(|| "https://sts.cn-north-4.myhuaweicloud.com".to_string());
    (base_url, sts_url)
}

/// Persist a fresh quota snapshot after a successful check-in.
///
/// The hook is provider agnostic: it asks the registered provider for the
/// account's quota and stores the result. Providers without quota support
/// return `Unsupported`, which is reported as a failed refresh without rolling
/// the check-in back.
struct LiveQuotaRefresh {
    accounts: AccountService,
    registry: Arc<ProviderRegistry>,
    storage: Storage,
}

impl LiveQuotaRefresh {
    fn new(accounts: AccountService, registry: Arc<ProviderRegistry>, storage: Storage) -> Self {
        Self {
            accounts,
            registry,
            storage,
        }
    }
}

#[async_trait]
impl QuotaRefreshHook for LiveQuotaRefresh {
    async fn refresh(&self, request: &QuotaRefreshRequest) -> Result<(), GatewayError> {
        let context: AuthorizationContext = self
            .accounts
            .authorization_context_for(&request.account_id, request.kind, request.region)
            .await?;
        let credential = context.credential.ok_or_else(|| {
            GatewayError::new(ErrorKind::AuthInvalid, "account has no credential")
        })?;
        let provider = self
            .registry
            .get(&ProviderKey::new(request.kind, request.region))
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::Unsupported,
                    format!(
                        "provider {}-{} is not registered",
                        request.kind.as_str(),
                        request.region.as_str()
                    ),
                )
            })?;
        if !provider.capabilities().quota {
            return Err(GatewayError::new(
                ErrorKind::Unsupported,
                "provider does not report quota",
            ));
        }
        let provider_context = crate::providers::ProviderContext {
            account_id: Some(request.account_id.clone()),
            credential_revision: context.credential_revision,
            ..Default::default()
        };
        let snapshot = provider
            .quota_with_context(&provider_context, &credential)
            .await?;
        let snapshot = QuotaSnapshot {
            account_id: request.account_id.clone(),
            ..snapshot
        };
        self.storage.quota().save(&snapshot).await?;
        Ok(())
    }
}

/// Freshen an expiring credential before a check-in runs.
///
/// OfficeAce signs quota and check-in calls with a temporary AK/SK that lives
/// about two hours. A daily claim against the stored copy is then rejected with
/// `APIG.0602` ("the security token has expired"), which is what the 2026-10-30
/// run hit. `auth_status_with_context` rotates that half when it is close to
/// expiry and is a no-op probe elsewhere.
struct LiveAuthRefresh {
    accounts: AccountService,
    registry: Arc<ProviderRegistry>,
}

impl LiveAuthRefresh {
    fn new(accounts: AccountService, registry: Arc<ProviderRegistry>) -> Self {
        Self { accounts, registry }
    }
}

#[async_trait]
impl AuthRefreshHook for LiveAuthRefresh {
    async fn refresh(&self, request: &AuthRefreshRequest) -> Result<(), GatewayError> {
        let context = self
            .accounts
            .authorization_context_for(&request.account_id, request.kind, request.region)
            .await?;
        let credential = context.credential.ok_or_else(|| {
            GatewayError::new(ErrorKind::AuthInvalid, "account has no credential")
        })?;
        let provider = self
            .registry
            .get(&ProviderKey::new(request.kind, request.region))
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::Unsupported,
                    format!(
                        "provider {}-{} is not registered",
                        request.kind.as_str(),
                        request.region.as_str()
                    ),
                )
            })?;
        let provider_context = crate::providers::ProviderContext {
            account_id: Some(request.account_id.clone()),
            credential_revision: context.credential_revision,
            ..Default::default()
        };
        provider
            .auth_status_with_context(&provider_context, &credential)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct MockToolCallSpec {
    id: String,
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

/// Translate one configured provider entry into mock behaviour.
///
/// Two provider options are understood, both development-only:
/// `mock.reply` (fixed reply text) and `mock.tool_calls` (JSON array of
/// `{id, name, arguments}`). Model aliases/`raw_id` are not mapped yet; the
/// configured id is used as both the public and upstream model id.
fn mock_config(provider: &ProviderConfig) -> Result<MockConfig, GatewayError> {
    let mut config = MockConfig::new(provider.provider_id(), provider.region);
    config.display_name = provider.display_name();
    config.enabled = provider.enabled;

    let models: Vec<UpstreamModel> = provider
        .models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| {
            let mut upstream = UpstreamModel::new(model.id.clone());
            upstream.display_name = Some(
                model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.id.clone()),
            );
            upstream.context_window = model.context_window;
            upstream.max_output = model.max_output;
            upstream
        })
        .collect();
    if !models.is_empty() {
        config.models = models;
    }

    if let Some(reply) = provider.options.get("mock.reply") {
        config.reply = MockReply::fixed(reply.clone());
    }
    if let Some(raw) = provider.options.get("mock.tool_calls") {
        let calls: Vec<MockToolCallSpec> = serde_json::from_str(raw).map_err(|e| {
            GatewayError::config(format!(
                "invalid mock.tool_calls for provider {}: {e}",
                provider.provider_id()
            ))
        })?;
        config.tool_calls = calls
            .into_iter()
            .map(|call| MockToolCall::new(call.id, call.name, call.arguments))
            .collect();
    }
    Ok(config)
}
