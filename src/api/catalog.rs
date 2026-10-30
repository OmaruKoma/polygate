//! Model catalogue exposed through `GET /v1/models`.
//!
//! The catalogue is built from the *enabled* providers in the registry by
//! asking each one that declares `model_discovery` for its model list. It never
//! invents models, and entries disappear as soon as their provider is disabled.
//!
//! M8 has no account store, so discovery happens once at router construction
//! with a placeholder credential. Providers that need a real credential will
//! report a discovery error (logged, entry skipped) until the account pool is
//! wired in a later stage.

use crate::account_service::AccountService;
use crate::accounts::Credential;
use crate::config::ProviderKind;
use crate::providers::{ProviderContext, ProviderKey, ProviderRegistry};

/// One model reachable through the gateway.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// Public model id clients send.
    pub id: String,
    /// Provider-side model id passed to the adapter.
    pub upstream_id: String,
    /// Provider instance that serves it.
    pub provider: ProviderKey,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    entries: Vec<ModelEntry>,
    discovered_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ModelCatalog {
    pub fn new(entries: Vec<ModelEntry>) -> Self {
        Self {
            entries,
            discovered_at: Some(chrono::Utc::now()),
        }
    }

    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    pub fn discovered_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.discovered_at
    }

    /// Discover models from every enabled provider that declares
    /// `model_discovery`. Discovery failures are logged and skipped; they never
    /// remove other providers' entries.
    pub async fn discover(registry: &ProviderRegistry, credential: &Credential) -> Self {
        let mut entries = Vec::new();
        for info in registry.list_enabled() {
            let key = info.key();
            let Some(provider) = registry.get(&key) else {
                continue;
            };
            if !provider.capabilities().model_discovery {
                continue;
            }
            match provider.list_models(credential).await {
                Ok(models) => Self::push_models(&mut entries, key, models),
                Err(err) => err.log("model_discovery"),
            }
        }
        let mut catalog = Self::finish(entries);
        catalog.discovered_at = Some(chrono::Utc::now());
        catalog
    }

    /// Account-aware discovery: every non-mock provider is asked with the
    /// credential of its first available account. A provider without a usable
    /// account contributes no models instead of an invented catalogue.
    pub async fn discover_with_accounts(
        registry: &ProviderRegistry,
        accounts: &AccountService,
    ) -> Self {
        let mut entries = Vec::new();
        for info in registry.list_enabled() {
            let key = info.key();
            let Some(provider) = registry.get(&key) else {
                continue;
            };
            if !provider.capabilities().model_discovery {
                continue;
            }
            let credential = if key.kind == ProviderKind::Mock {
                Credential::from_value(serde_json::json!({}))
            } else {
                let Ok(mut available) = accounts.list_available(key.kind, key.region).await else {
                    continue;
                };
                available.sort_by(|a, b| a.id.cmp(&b.id));
                let Some(account) = available.into_iter().next() else {
                    continue;
                };
                let Ok(context) = accounts.authorization_context(&account.id).await else {
                    continue;
                };
                let Some(credential) = context.credential else {
                    continue;
                };
                // Let an expiring credential rotate itself first: an STS token
                // that died while the process was down would otherwise remove
                // every model of that provider from the catalogue. This is the
                // same path a real request takes, and the refreshed credential
                // is persisted by the adapter.
                let probe = ProviderContext {
                    account_id: Some(account.id.clone()),
                    credential_revision: context.credential_revision,
                    ..Default::default()
                };
                let _ = provider.auth_status_with_context(&probe, &credential).await;
                accounts
                    .authorization_context(&account.id)
                    .await
                    .ok()
                    .and_then(|fresh| fresh.credential)
                    .unwrap_or(credential)
            };
            match provider.list_models(&credential).await {
                Ok(models) => Self::push_models(&mut entries, key, models),
                Err(err) => {
                    // Name the provider and the reason: a silently missing
                    // provider is the hardest kind of catalogue gap to explain.
                    tracing::warn!(
                        provider = %info.id,
                        kind = ?err.kind,
                        detail = %err.message,
                        "model discovery failed"
                    );
                }
            }
        }
        let mut catalog = Self::finish(entries);
        catalog.discovered_at = Some(chrono::Utc::now());
        catalog
    }

    fn push_models(
        entries: &mut Vec<ModelEntry>,
        key: ProviderKey,
        models: Vec<crate::providers::UpstreamModel>,
    ) {
        for model in models {
            if model.id.trim().is_empty() {
                continue;
            }
            entries.push(ModelEntry {
                id: model.id.clone(),
                upstream_id: model.id,
                provider: key,
                display_name: model.display_name,
                context_window: model.context_window,
                max_output: model.max_output,
            });
        }
    }

    fn finish(mut entries: Vec<ModelEntry>) -> Self {
        entries.sort_by(|a, b| {
            a.id.cmp(&b.id)
                .then_with(|| a.provider.to_string().cmp(&b.provider.to_string()))
        });
        Self {
            entries,
            discovered_at: Some(chrono::Utc::now()),
        }
    }

    /// Resolve a public model id to an enabled provider. Disabled providers are
    /// not routable even if they were enabled at discovery time.
    pub fn resolve<'a>(
        &'a self,
        registry: &ProviderRegistry,
        model: &str,
    ) -> Option<&'a ModelEntry> {
        self.entries
            .iter()
            .find(|entry| entry.id == model && registry.is_enabled(&entry.provider))
    }

    /// Entries of providers that are currently enabled.
    pub fn enabled<'a>(&'a self, registry: &ProviderRegistry) -> Vec<&'a ModelEntry> {
        self.entries
            .iter()
            .filter(|entry| registry.is_enabled(&entry.provider))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
