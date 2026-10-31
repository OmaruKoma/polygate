//! Credential freshening hook, run right before a check-in executor.
//!
//! OfficeAce signs quota and check-in calls with a temporary AK/SK that lives
//! about two hours. The stored credential is not touched between check-ins, so
//! a daily claim could present an expired `X-Security-Token` and fail with
//! `APIG.0602`. The provider already knows how to rotate that half (it does so
//! before serving a request); this hook lets the check-in engine ask for the
//! same refresh without depending on the provider registry.
//!
//! A failure here is not fatal: the executor still runs and reports its own
//! error category, which keeps "cannot refresh" distinct from "claim refused".

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;

use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRefreshRequest {
    pub account_id: String,
    pub kind: ProviderKind,
    pub region: Region,
}

#[async_trait]
pub trait AuthRefreshHook: Send + Sync {
    /// Refresh an expiring credential in place. The rotated material is
    /// persisted by the provider; the check-in engine reloads it afterwards.
    async fn refresh(&self, request: &AuthRefreshRequest) -> Result<(), GatewayError>;
}

/// Default: do nothing. Used by tests and by builds without a provider registry.
#[derive(Debug, Default)]
pub struct NoopAuthRefresh;

#[async_trait]
impl AuthRefreshHook for NoopAuthRefresh {
    async fn refresh(&self, _request: &AuthRefreshRequest) -> Result<(), GatewayError> {
        Ok(())
    }
}

/// Records calls and can be switched to fail, for tests of failure isolation.
#[derive(Default)]
pub struct MockAuthRefreshHook {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl MockAuthRefreshHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthRefreshHook for MockAuthRefreshHook {
    async fn refresh(&self, _request: &AuthRefreshRequest) -> Result<(), GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(GatewayError::transport("mock auth refresh failed"));
        }
        Ok(())
    }
}
