//! Background check-in loop.
//!
//! The loop itself holds no per-day state. "What is due" is recomputed from the
//! persisted schedules and the persisted runs every tick, so a restart resumes
//! exactly where the database says it stopped. Ownership of one account/day is
//! decided by the SQLite claim, which means two schedulers (for example during a
//! rolling restart) cannot both execute the same account.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::GatewayError;

use super::{CheckinResult, CheckinService, DEFAULT_TICK_SECS};

/// Sleep abstraction so tests can drive the loop without wall-clock waits.
#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug, Default)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[derive(Clone)]
pub struct CheckinScheduler {
    service: CheckinService,
    interval: Duration,
    sleeper: Arc<dyn Sleeper>,
}

impl CheckinScheduler {
    pub fn new(service: CheckinService) -> Self {
        Self {
            service,
            interval: Duration::from_secs(DEFAULT_TICK_SECS),
            sleeper: Arc::new(TokioSleeper),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    pub fn service(&self) -> &CheckinService {
        &self.service
    }

    /// One pass over the schedules. Exposed separately so tests and an eventual
    /// admin endpoint can trigger it deterministically.
    pub async fn tick(&self) -> Result<Vec<CheckinResult>, GatewayError> {
        self.service.run_due().await
    }

    /// Run until `shutdown` resolves.
    ///
    /// On start, runs whose worker died are resolved (status probe or `unknown`),
    /// never re-executed. Tick errors are logged and do not stop the loop. On
    /// shutdown the loop returns without starting new work; the in-flight
    /// accounts finish before `run_due` returns.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) {
        tokio::pin!(shutdown);

        if let Err(error) = self.service.recover_interrupted().await {
            error.log("checkin_recovery");
        }

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!("check-in scheduler stopped");
                    return;
                }
                _ = self.sleeper.sleep(self.interval) => {
                    if let Err(error) = self.service.run_due().await {
                        error.log("checkin_scheduler");
                    }
                }
            }
        }
    }
}
