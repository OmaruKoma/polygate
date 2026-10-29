//! Bounded account and session concurrency.
//!
//! CodeArts counts *upstream sessions*, not just in-flight HTTP requests: two
//! of the three reference implementations record that an account is limited to
//! three sessions and that a session lingers for tens of seconds after the
//! stream ends, so three quick requests followed by three more can still trip
//! the cap. The limiter therefore combines:
//!
//! * an in-flight semaphore (`max_in_flight`),
//! * a sliding start window (`max_starts` per `window`),
//! * one optional serialization lock per downstream session.
//!
//! Every acquisition has one absolute deadline (`queue_timeout`); waiting is
//! bounded and cancellation-safe (dropping the future drops the permit).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep_until, timeout_at, Instant};

use crate::error::{ErrorKind, GatewayError};

/// Limits applied to one account (and optionally one downstream session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitPolicy {
    pub max_in_flight: usize,
    /// Sliding-window start budget; `0` disables the window.
    pub max_starts: usize,
    /// Length of the start window; ignored when `max_starts == 0`.
    pub window: Duration,
    /// Upper bound for the whole acquisition (semaphore + window wait).
    pub queue_timeout: Duration,
    /// Serialize requests that share one downstream session (and therefore one
    /// upstream session id). Different sessions are never blocked by each other.
    pub serialize_session: bool,
}

impl LimitPolicy {
    /// Conservative policy for CodeArts, derived from the two reference
    /// implementations (see `docs/CODEARTS_SESSION.md`): three sessions per
    /// account and a roughly 60-second session lifetime, no unbounded queuing.
    pub fn codearts() -> Self {
        Self {
            max_in_flight: 3,
            max_starts: 3,
            window: Duration::from_secs(60),
            // Slightly longer than the observed ~60s session linger, so the
            // fourth burst request waits instead of failing immediately.
            queue_timeout: Duration::from_secs(75),
            serialize_session: true,
        }
    }

    /// Generic account limit: only the operator-configured account concurrency
    /// and no window/wall assumptions.
    pub fn from_account(max_concurrency: u32) -> Self {
        Self {
            max_in_flight: usize::try_from(max_concurrency).unwrap_or(1).max(1),
            max_starts: 0,
            window: Duration::ZERO,
            queue_timeout: Duration::from_secs(30),
            serialize_session: false,
        }
    }

    pub fn validate(self) -> Result<Self, GatewayError> {
        if self.max_in_flight == 0 {
            return Err(GatewayError::config(
                "account max_in_flight must be at least 1",
            ));
        }
        if self.max_starts > 0 && self.window.is_zero() {
            return Err(GatewayError::config(
                "account session window must be positive when max_starts is set",
            ));
        }
        if self.queue_timeout.is_zero() {
            return Err(GatewayError::config(
                "account queue timeout must be positive",
            ));
        }
        Ok(self)
    }
}

struct AccountState {
    semaphore: Arc<Semaphore>,
    starts: AsyncMutex<VecDeque<Instant>>,
}

/// Per-account limiter shared by every request through one gateway process.
#[derive(Clone)]
pub struct AccountLimiter {
    policy: LimitPolicy,
    states: Arc<Mutex<HashMap<String, Arc<AccountState>>>>,
}

impl AccountLimiter {
    pub fn new(policy: LimitPolicy) -> Self {
        Self {
            policy,
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn policy(&self) -> LimitPolicy {
        self.policy
    }

    /// Acquire one account slot before the absolute `deadline`.
    ///
    /// The returned lease releases the in-flight permit on drop. Window entries
    /// are timestamps and expire naturally; they are not refunded on release,
    /// which is the point (upstream sessions linger after the request ends).
    pub async fn acquire(
        &self,
        account_id: &str,
        operator_limit: usize,
        deadline: Instant,
    ) -> Result<AccountLease, GatewayError> {
        let state = self.state(account_id, operator_limit);
        let permit = match timeout_at(deadline, state.semaphore.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => {
                return Err(GatewayError::internal("account limiter was closed"));
            }
            Err(_) => return Err(busy(account_id, "in-flight")),
        };

        if self.policy.max_starts > 0 {
            let mut starts = state.starts.lock().await;
            loop {
                let now = Instant::now();
                while starts
                    .front()
                    .is_some_and(|started| *started + self.policy.window <= now)
                {
                    starts.pop_front();
                }
                if starts.len() < self.policy.max_starts {
                    starts.push_back(now);
                    break;
                }
                let next_slot = starts
                    .front()
                    .copied()
                    .expect("a full window has a first start")
                    + self.policy.window;
                if next_slot >= deadline {
                    return Err(busy(account_id, "session window"));
                }
                sleep_until(next_slot).await;
            }
        }

        Ok(AccountLease { _permit: permit })
    }

    fn state(&self, account_id: &str, operator_limit: usize) -> Arc<AccountState> {
        // The CodeArts cap is an upper bound; an operator may configure a
        // smaller `accounts.max_concurrency`. The capacity is fixed at first
        // use, so a runtime change needs a restart or a future DB lease.
        let capacity = self.policy.max_in_flight.min(operator_limit.max(1));
        let mut states = self.states.lock();
        states
            .entry(account_id.to_string())
            .or_insert_with(|| {
                Arc::new(AccountState {
                    semaphore: Arc::new(Semaphore::new(capacity)),
                    starts: AsyncMutex::new(VecDeque::new()),
                })
            })
            .clone()
    }
}

/// Held for the duration of one upstream request. Dropping it releases the
/// account in-flight slot.
pub struct AccountLease {
    _permit: OwnedSemaphorePermit,
}

/// One lock per downstream session, created lazily and cleaned up when no
/// waiter/guard holds it anymore.
#[derive(Clone, Default)]
pub struct SessionGate {
    locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

impl SessionGate {
    pub async fn acquire(
        &self,
        key: &str,
        deadline: Instant,
    ) -> Result<SessionGateGuard, GatewayError> {
        let lock = {
            let mut locks = self.locks.lock();
            locks.retain(|_, weak| weak.strong_count() > 0);
            match locks.get(key).and_then(Weak::upgrade) {
                Some(existing) => existing,
                None => {
                    let created = Arc::new(AsyncMutex::new(()));
                    locks.insert(key.to_string(), Arc::downgrade(&created));
                    created
                }
            }
        };
        match timeout_at(deadline, lock.lock_owned()).await {
            Ok(guard) => Ok(SessionGateGuard { _guard: guard }),
            Err(_) => Err(GatewayError::new(
                ErrorKind::NoCapacity,
                "timed out waiting for the previous request of this session",
            )),
        }
    }
}

pub struct SessionGateGuard {
    _guard: OwnedMutexGuard<()>,
}

fn busy(account_id: &str, scope: &str) -> GatewayError {
    GatewayError::new(
        ErrorKind::NoCapacity,
        format!("account {account_id} reached its upstream {scope} limit; queue timed out"),
    )
}
