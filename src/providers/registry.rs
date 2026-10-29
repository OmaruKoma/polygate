//! Concurrent `(kind, region)` → provider registry.
//!
//! The registry owns only handles and enable flags. It never holds credentials,
//! never judges account health and never picks an account: that is the
//! scheduler's job. Callers resolve a provider here and pass the decrypted
//! credential along with each call.

use std::collections::HashMap;

use parking_lot::RwLock;

use super::{
    unsupported_capability, Capabilities, Capability, ProviderInfo, ProviderKey, ProviderRef,
    VerificationState,
};
use crate::error::GatewayError;

struct RegistryEntry {
    provider: ProviderRef,
    enabled: bool,
}

/// Thread safe provider registry. All reads take a shared lock; registration
/// and enable/disable take the exclusive lock, so callers can resolve a
/// provider on every request without cloning the registry.
#[derive(Default)]
pub struct ProviderRegistry {
    entries: RwLock<HashMap<ProviderKey, RegistryEntry>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one provider. Rejects a duplicate key or a duplicate instance id.
    ///
    /// The returned [`ProviderInfo`] reflects the state stored in the registry
    /// (including the initial `enabled` flag).
    pub fn register(&self, provider: ProviderRef) -> Result<ProviderInfo, GatewayError> {
        let info = provider.info();
        let key = info.key();
        if info.id.trim().is_empty() {
            return Err(GatewayError::config(format!(
                "provider {key} reports an empty instance id"
            )));
        }

        let mut entries = self.entries.write();
        if entries.contains_key(&key) {
            return Err(GatewayError::config(format!(
                "provider {key} is already registered"
            )));
        }
        if let Some(existing) = entries.values().find(|e| e.provider.info().id == info.id) {
            let other = existing.provider.info().key();
            return Err(GatewayError::config(format!(
                "provider id {} is already used by {other}",
                info.id
            )));
        }

        entries.insert(
            key,
            RegistryEntry {
                provider,
                enabled: info.enabled,
            },
        );
        Ok(info)
    }

    /// Register several providers; aborts on the first conflict without
    /// rolling back the ones already inserted.
    pub fn register_all(
        &self,
        providers: impl IntoIterator<Item = ProviderRef>,
    ) -> Result<usize, GatewayError> {
        let mut count = 0;
        for provider in providers {
            self.register(provider)?;
            count += 1;
        }
        Ok(count)
    }

    /// Look up a provider regardless of its enabled state (admin/inspection).
    pub fn get(&self, key: &ProviderKey) -> Option<ProviderRef> {
        self.entries
            .read()
            .get(key)
            .map(|entry| entry.provider.clone())
    }

    /// Look up a provider instance by its configured id (for example
    /// `trae-cn`), regardless of the enable flag.
    pub fn info_by_id(&self, id: &str) -> Option<ProviderInfo> {
        let entries = self.entries.read();
        entries
            .values()
            .map(|entry| entry.provider.info())
            .find(|info| info.id == id)
    }

    /// Look up a provider that may serve requests: registered and enabled.
    pub fn require(&self, key: &ProviderKey) -> Result<ProviderRef, GatewayError> {
        let entries = self.entries.read();
        match entries.get(key) {
            None => Err(GatewayError::model_unavailable(format!(
                "provider {key} is not registered"
            ))),
            Some(entry) if !entry.enabled => Err(GatewayError::no_capacity(format!(
                "provider {key} is disabled"
            ))),
            Some(entry) => Ok(entry.provider.clone()),
        }
    }

    /// Look up a provider and fail with `Unsupported` when the capability is not
    /// declared by the implementation.
    pub fn require_capability(
        &self,
        key: &ProviderKey,
        capability: Capability,
    ) -> Result<ProviderRef, GatewayError> {
        let provider = self.require(key)?;
        let capabilities = provider.capabilities();
        if !capabilities.get(capability) {
            return Err(unsupported_capability(&provider.info(), capability));
        }
        Ok(provider)
    }

    /// Enable or disable a registered provider. Disabling only removes the
    /// provider from new-request resolution; its accounts and records stay.
    pub fn set_enabled(&self, key: &ProviderKey, enabled: bool) -> Result<(), GatewayError> {
        let mut entries = self.entries.write();
        match entries.get_mut(key) {
            None => Err(GatewayError::model_unavailable(format!(
                "provider {key} is not registered"
            ))),
            Some(entry) => {
                entry.enabled = enabled;
                Ok(())
            }
        }
    }

    pub fn is_enabled(&self, key: &ProviderKey) -> bool {
        self.entries
            .read()
            .get(key)
            .map(|entry| entry.enabled)
            .unwrap_or(false)
    }

    /// Capability snapshot of a registered provider, enabled or not.
    pub fn capabilities(&self, key: &ProviderKey) -> Option<Capabilities> {
        self.entries
            .read()
            .get(key)
            .map(|entry| entry.provider.capabilities())
    }

    /// Verification state of a registered provider, enabled or not.
    pub fn verification(&self, key: &ProviderKey) -> Option<VerificationState> {
        self.entries
            .read()
            .get(key)
            .map(|entry| entry.provider.info().verification)
    }

    /// Default listing: only providers that may serve requests.
    pub fn list_enabled(&self) -> Vec<ProviderInfo> {
        self.list(|entry| entry.enabled)
    }

    /// Every registered provider, including disabled ones (admin view).
    pub fn list_all(&self) -> Vec<ProviderInfo> {
        self.list(|_| true)
    }

    fn list(&self, keep: impl Fn(&RegistryEntry) -> bool) -> Vec<ProviderInfo> {
        let entries = self.entries.read();
        let mut out: Vec<ProviderInfo> = entries
            .iter()
            .filter(|(_, entry)| keep(entry))
            .map(|(key, entry)| {
                let mut info = entry.provider.info();
                info.enabled = entry.enabled;
                info.id = if info.id.is_empty() {
                    key.to_string()
                } else {
                    info.id
                };
                info
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn keys(&self) -> Vec<ProviderKey> {
        let entries = self.entries.read();
        let mut keys: Vec<ProviderKey> = entries.keys().copied().collect();
        keys.sort_by_key(|k| k.to_string());
        keys
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}
