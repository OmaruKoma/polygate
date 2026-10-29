//! Accounts and credentials.
//!
//! An account is a configured authorization of one provider. Its secret is a
//! [`Credential`] that is never serialized without going through
//! [`crate::security::SecretBox`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::{ProviderKind, Region};

/// Authentication state of one account.
///
/// This is deliberately separate from [`Account::enabled`]: disabling an
/// account is a local operator action, while `AuthStatus` describes what the
/// upstream told us (or has not told us yet). A disabled account keeps its
/// credential and its auth status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    /// Never probed, or explicitly reset.
    Unknown,
    /// Last probe/refresh succeeded.
    Valid,
    /// Credential expired; a refresh or a new login is required.
    Expired,
    /// Upstream rejected the credential permanently.
    Invalid,
    /// Credential works only after a refresh; requests should refresh first.
    RefreshRequired,
    /// Upstream applies a channel/account restriction (not necessarily a dead
    /// credential, but not usable for normal dispatch either).
    Restricted,
}

impl AuthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthStatus::Unknown => "unknown",
            AuthStatus::Valid => "valid",
            AuthStatus::Expired => "expired",
            AuthStatus::Invalid => "invalid",
            AuthStatus::RefreshRequired => "refresh_required",
            AuthStatus::Restricted => "restricted",
        }
    }

    /// Read legacy values written by earlier M4 tests/DBs. `ok` was the old
    /// name for `valid`; `disabled` was an auth-level value and is folded into
    /// `invalid` so it never gets confused with the local `enabled` flag.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "valid" | "ok" => AuthStatus::Valid,
            "expired" => AuthStatus::Expired,
            "invalid" | "disabled" => AuthStatus::Invalid,
            "refresh_required" => AuthStatus::RefreshRequired,
            "restricted" => AuthStatus::Restricted,
            _ => AuthStatus::Unknown,
        }
    }

    /// Whether an account in this auth state may be offered to the scheduler
    /// without a refresh. `Unknown` is included because a new account has not
    /// been probed yet; excluding it would strand every freshly imported
    /// credential. `Expired`/`RefreshRequired` must be refreshed first.
    pub const fn is_dispatchable(self) -> bool {
        matches!(self, AuthStatus::Unknown | AuthStatus::Valid)
    }

    pub const fn requires_refresh(self) -> bool {
        matches!(self, AuthStatus::Expired | AuthStatus::RefreshRequired)
    }

    /// Allowed state transitions. Same-state updates are idempotent. Every
    /// state may be explicitly reset to `Unknown` (e.g. after re-import).
    /// `Invalid` can only become `Valid` (re-authenticated) or `Unknown`; an
    /// auth-rejected credential must not silently turn into "expired".
    pub fn can_transition_to(self, next: Self) -> bool {
        if matches!(next, AuthStatus::Unknown) || self == next {
            return true;
        }
        match self {
            AuthStatus::Unknown => true,
            AuthStatus::Valid => matches!(
                next,
                AuthStatus::Expired
                    | AuthStatus::Invalid
                    | AuthStatus::RefreshRequired
                    | AuthStatus::Restricted
            ),
            AuthStatus::Expired => matches!(
                next,
                AuthStatus::Valid
                    | AuthStatus::Invalid
                    | AuthStatus::RefreshRequired
                    | AuthStatus::Restricted
            ),
            AuthStatus::RefreshRequired => matches!(
                next,
                AuthStatus::Valid
                    | AuthStatus::Expired
                    | AuthStatus::Invalid
                    | AuthStatus::Restricted
            ),
            AuthStatus::Invalid => matches!(next, AuthStatus::Valid),
            AuthStatus::Restricted => matches!(
                next,
                AuthStatus::Valid
                    | AuthStatus::Expired
                    | AuthStatus::Invalid
                    | AuthStatus::RefreshRequired
            ),
        }
    }
}

/// Opaque credential payload (provider specific keys, e.g. `token`, `cookie`,
/// `refresh_token`, `user_id`). `Debug` redacts the content and `Drop` zeroizes
/// string values on a best-effort basis.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credential(pub serde_json::Value);

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

impl Drop for Credential {
    fn drop(&mut self) {
        zeroize_value(&mut self.0);
    }
}

fn zeroize_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(zeroize_value),
        serde_json::Value::Object(map) => map.values_mut().for_each(zeroize_value),
        _ => {}
    }
}

impl Credential {
    pub fn from_value(v: serde_json::Value) -> Self {
        Self(v)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }
    /// Required field or a classified error useful to the admin UI.
    pub fn require(&self, key: &str) -> Result<&str, crate::error::GatewayError> {
        self.get(key).filter(|s| !s.is_empty()).ok_or_else(|| {
            crate::error::GatewayError::new(
                crate::error::ErrorKind::AuthInvalid,
                format!("credential is missing required field `{key}`"),
            )
        })
    }
    pub fn to_json(&self) -> &serde_json::Value {
        &self.0
    }
    pub fn hint(&self) -> String {
        crate::security::credential_hint(&self.0)
    }
}

/// Persisted account metadata (without the secret).
#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub name: String,
    pub credential_hint: String,
    /// Stable identity supplied by an import source (for example
    /// `codearts2api:<domain>:<user>`). `None` for accounts created directly
    /// through the API; imports use it to stay idempotent.
    pub external_id: Option<String>,
    pub enabled: bool,
    pub weight: u32,
    pub max_concurrency: u32,
    pub quota_group: Option<String>,
    pub auth_status: AuthStatus,
    pub auth_checked_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Deserialize)]
pub struct NewAccount {
    pub provider: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub region: Option<Region>,
    pub name: String,
    /// Raw credential JSON (encrypted on write).
    pub credential: serde_json::Value,
    /// Import identity; unique within `(kind, region)` when present.
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub quota_group: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl std::fmt::Debug for NewAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewAccount")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("region", &self.region)
            .field("name", &self.name)
            .field("credential", &"<redacted>")
            .field("external_id", &self.external_id)
            .field("weight", &self.weight)
            .field("max_concurrency", &self.max_concurrency)
            .field("quota_group", &self.quota_group)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

fn default_weight() -> u32 {
    1
}
fn default_concurrency() -> u32 {
    4
}
fn default_true() -> bool {
    true
}

/// Public account view for the admin API (never contains the secret).
#[derive(Debug, Serialize)]
pub struct AccountView {
    #[serde(flatten)]
    pub account: Account,
    pub runtime: AccountRuntimeView,
}

/// Runtime scheduling state.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountRuntimeView {
    pub inflight: u32,
    pub healthy: bool,
    pub cooling_until: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub quota_exhausted: bool,
    pub sessions: u64,
    pub requests: u64,
}
