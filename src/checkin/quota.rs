//! Quota refresh hook.
//!
//! M12 does not query real quota. The hook exists so a successful check-in can
//! express "please refresh account X" without the check-in engine knowing how.
//! A failure here is recorded in the result and is never allowed to roll a
//! successful check-in back to `Failed`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::NaiveDate;

use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRefreshRequest {
    pub account_id: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub business_date: NaiveDate,
}

#[async_trait]
pub trait QuotaRefreshHook: Send + Sync {
    /// Queue or perform a quota refresh. Returning an error only marks this
    /// side-effect as failed; the check-in status stays as persisted.
    async fn refresh(&self, request: &QuotaRefreshRequest) -> Result<(), GatewayError>;
}

/// Records calls and can be switched to fail, for tests of failure isolation.
#[derive(Default)]
pub struct MockQuotaRefreshHook {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl MockQuotaRefreshHook {
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
impl QuotaRefreshHook for MockQuotaRefreshHook {
    async fn refresh(&self, _request: &QuotaRefreshRequest) -> Result<(), GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(GatewayError::transport("mock quota refresh failed"));
        }
        Ok(())
    }
}
