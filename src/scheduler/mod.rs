//! Account scheduling.
//!
//! The scheduler only ever sees accounts that already passed
//! [`AccountService::list_available`] (enabled, dispatchable auth state, stored
//! credential). Its job is to order those candidates and to remember runtime
//! health:
//!
//! * `round_robin` — strict rotation per `(kind, region)`;
//! * `smooth_weighted_round_robin` (default) — nginx-style SWRR using the
//!   operator-configured account weight;
//! * `least_connections` — fewest in-flight requests.
//!
//! Constraints are applied before any strategy:
//!
//! * a known-exhausted quota (`remaining <= 0`) excludes the account; unknown
//!   quota never does;
//! * `quota_min_ratio > 0` excludes accounts below that ratio;
//! * accounts in cooldown after repeated transient failures are skipped.
//!
//! Failures are classified conservatively: only rate limits, upstream 5xx and
//! transport failures cool an account down. Client errors, quota exhaustion and
//! auth rejections do not (auth state is owned by `AccountService`).
//!
//! All state is in-process and bounded by the number of configured accounts;
//! there is no background task and no global async lock.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;

use crate::accounts::Account;
use crate::config::{ProviderKind, Region, SchedulerConfig, SchedulerStrategy};
use crate::error::{ErrorKind, GatewayError};
use crate::quota::QuotaStatus;
use crate::storage::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The request may be retried elsewhere without duplicating work (429/402/
    /// auth rejection/no local capacity).
    Retryable,
    /// Uncertain outcome (timeout/transport/5xx). Never retried automatically.
    Uncertain,
    /// Client-side or configuration problem; switching accounts cannot help.
    Client,
}

pub fn classify_failure(kind: ErrorKind) -> FailureClass {
    match kind {
        ErrorKind::AuthExpired
        | ErrorKind::AuthInvalid
        | ErrorKind::AccountDisabled
        | ErrorKind::ChannelRestricted
        | ErrorKind::RateLimited
        | ErrorKind::QuotaExhausted
        | ErrorKind::NoCapacity => FailureClass::Retryable,
        ErrorKind::Transport | ErrorKind::Timeout | ErrorKind::UpstreamServer => {
            FailureClass::Uncertain
        }
        _ => FailureClass::Client,
    }
}

/// Reasons an account was filtered out; kept for the no-capacity error so an
/// operator can see why every candidate was skipped.
#[derive(Debug, Default)]
struct Rejections {
    reasons: Vec<String>,
}

impl Rejections {
    fn push(&mut self, account: &Account, reason: impl Into<String>) {
        self.reasons
            .push(format!("{}: {}", account.id, reason.into()));
    }

    fn into_message(self) -> String {
        if self.reasons.is_empty() {
            "no candidate account".to_string()
        } else {
            self.reasons.join("; ")
        }
    }
}

#[derive(Debug, Default)]
struct AccountState {
    inflight: u32,
    consecutive_failures: u32,
    current_weight: i64,
    cooldown_until: Option<i64>,
}

#[derive(Default)]
struct State {
    accounts: HashMap<String, AccountState>,
    rotation: HashMap<(ProviderKind, Region), usize>,
}

pub struct Scheduler {
    config: SchedulerConfig,
    storage: Option<Storage>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("strategy", &self.config.strategy)
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    pub fn new(config: SchedulerConfig, storage: Option<Storage>) -> Self {
        Self {
            config,
            storage,
            state: Mutex::new(State::default()),
        }
    }

    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Choose one account. `candidates` must already be dispatchable; this
    /// method only applies quota/cooldown filters and the strategy.
    pub async fn select(
        &self,
        kind: ProviderKind,
        region: Region,
        candidates: &[Account],
    ) -> Result<Account, GatewayError> {
        if candidates.is_empty() {
            return Err(no_capacity(kind, region, "no available account"));
        }
        let now = Utc::now().timestamp();
        let mut rejections = Rejections::default();
        let mut eligible: Vec<&Account> = Vec::new();
        for account in candidates {
            if self.in_cooldown(&account.id, now) {
                rejections.push(account, "cooling down after repeated failures");
                continue;
            }
            if let Some(reason) = self.quota_exclusion(&account.id).await {
                rejections.push(account, reason);
                continue;
            }
            eligible.push(account);
        }
        if eligible.is_empty() {
            return Err(no_capacity(kind, region, &rejections.into_message()));
        }
        eligible.sort_by(|a, b| a.id.cmp(&b.id));
        let picked = match self.config.strategy {
            SchedulerStrategy::RoundRobin => {
                let mut state = self.state.lock();
                let counter = state.rotation.entry((kind, region)).or_insert(0);
                let index = *counter % eligible.len();
                *counter = counter.wrapping_add(1);
                eligible[index].clone()
            }
            SchedulerStrategy::SmoothWeightedRoundRobin => {
                let mut state = self.state.lock();
                smooth_weighted(&mut state, &eligible)
            }
            SchedulerStrategy::LeastConnections => {
                let state = self.state.lock();
                eligible
                    .iter()
                    .min_by_key(|account| {
                        let inflight = state
                            .accounts
                            .get(&account.id)
                            .map(|entry| entry.inflight)
                            .unwrap_or(0);
                        (inflight, account.id.clone())
                    })
                    .map(|account| (*account).clone())
                    .expect("eligible is not empty")
            }
        };
        Ok(picked)
    }

    /// Record that a request now holds this account (called after the account
    /// concurrency permit is acquired).
    pub fn acquired(&self, account_id: &str) {
        let mut state = self.state.lock();
        state
            .accounts
            .entry(account_id.to_string())
            .or_default()
            .inflight += 1;
    }

    /// Release the in-flight slot and update health counters.
    pub fn release(&self, account_id: &str, failure: Option<ErrorKind>) {
        let now = Utc::now().timestamp();
        let mut state = self.state.lock();
        let entry = state.accounts.entry(account_id.to_string()).or_default();
        entry.inflight = entry.inflight.saturating_sub(1);
        match failure.map(classify_failure) {
            Some(FailureClass::Uncertain) => {
                entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
                if entry.consecutive_failures >= self.config.failure_threshold.max(1) {
                    let exponent = entry
                        .consecutive_failures
                        .saturating_sub(self.config.failure_threshold.max(1))
                        .min(16);
                    let base = self.config.cooldown_base_secs.max(1);
                    let cooldown = base
                        .saturating_mul(1u64 << exponent)
                        .min(self.config.cooldown_max_secs.max(base));
                    entry.cooldown_until = Some(now.saturating_add(cooldown as i64));
                }
            }
            Some(FailureClass::Retryable) | Some(FailureClass::Client) => {}
            None => {
                entry.consecutive_failures = 0;
                entry.cooldown_until = None;
            }
        }
    }

    /// Clear all remembered health (used when an operator re-enables an
    /// account or changes the strategy).
    pub fn reset_account(&self, account_id: &str) {
        self.state.lock().accounts.remove(account_id);
    }

    /// Snapshot for the admin dashboard/accounts view.
    pub fn runtime(&self, account_id: &str) -> RuntimeSnapshot {
        let now = Utc::now().timestamp();
        let state = self.state.lock();
        let entry = state.accounts.get(account_id);
        RuntimeSnapshot {
            inflight: entry.map(|entry| entry.inflight).unwrap_or(0),
            consecutive_failures: entry.map(|entry| entry.consecutive_failures).unwrap_or(0),
            cooling_until: entry
                .and_then(|entry| entry.cooldown_until)
                .filter(|until| *until > now),
        }
    }

    fn in_cooldown(&self, account_id: &str, now: i64) -> bool {
        let state = self.state.lock();
        state
            .accounts
            .get(account_id)
            .and_then(|entry| entry.cooldown_until)
            .is_some_and(|until| until > now)
    }

    async fn quota_exclusion(&self, account_id: &str) -> Option<String> {
        let storage = self.storage.as_ref()?;
        let snapshot = match storage.quota().latest_for_account(account_id).await {
            Ok(snapshot) => snapshot?,
            Err(_) => return None,
        };
        if snapshot.status != QuotaStatus::Known {
            return None;
        }
        if snapshot.remaining.is_some_and(|remaining| remaining <= 0.0) {
            return Some("known quota exhausted".to_string());
        }
        if self.config.quota_min_ratio > 0.0 {
            if let Some(ratio) = snapshot.ratio() {
                if ratio < self.config.quota_min_ratio {
                    return Some(format!(
                        "quota ratio {:.3} is below the configured minimum {:.3}",
                        ratio, self.config.quota_min_ratio
                    ));
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeSnapshot {
    pub inflight: u32,
    pub consecutive_failures: u32,
    pub cooling_until: Option<i64>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new(SchedulerConfig::default(), None)
    }
}

fn smooth_weighted(state: &mut State, eligible: &[&Account]) -> Account {
    let mut total: i64 = 0;
    let mut best: Option<(i64, &Account)> = None;
    for account in eligible {
        let weight = i64::from(account.weight.max(1));
        let entry = state.accounts.entry(account.id.clone()).or_default();
        entry.current_weight = entry.current_weight.saturating_add(weight);
        total = total.saturating_add(weight);
        let score = entry.current_weight;
        if best.map(|(current, _)| score > current).unwrap_or(true) {
            best = Some((score, account));
        }
    }
    let (_, winner) = best.expect("eligible is not empty");
    if let Some(entry) = state.accounts.get_mut(&winner.id) {
        entry.current_weight = entry.current_weight.saturating_sub(total);
    }
    (*winner).clone()
}

fn no_capacity(kind: ProviderKind, region: Region, detail: &str) -> GatewayError {
    GatewayError::new(
        ErrorKind::NoCapacity,
        format!(
            "no available {}-{} account: {detail}",
            kind.as_str(),
            region.as_str()
        ),
    )
}

/// Handle used by the runtime and tests.
pub type SharedScheduler = Arc<Scheduler>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{Account, AuthStatus};
    use chrono::Utc;

    fn account(id: &str, weight: u32) -> Account {
        let now = Utc::now();
        Account {
            id: id.to_string(),
            provider: format!("mock-{}", id),
            kind: ProviderKind::Mock,
            region: Region::Cn,
            name: id.to_string(),
            credential_hint: String::new(),
            external_id: None,
            enabled: true,
            weight,
            max_concurrency: 4,
            quota_group: None,
            auth_status: AuthStatus::Valid,
            auth_checked_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn round_robin_alternates() {
        let scheduler = Scheduler::new(
            SchedulerConfig {
                strategy: SchedulerStrategy::RoundRobin,
                ..SchedulerConfig::default()
            },
            None,
        );
        let candidates = vec![account("a", 1), account("b", 1)];
        let first = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        let second = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        let third = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(first.id, third.id);
    }

    #[tokio::test]
    async fn weighted_round_robin_favours_the_heavier_account() {
        let scheduler = Scheduler::new(
            SchedulerConfig {
                strategy: SchedulerStrategy::SmoothWeightedRoundRobin,
                ..SchedulerConfig::default()
            },
            None,
        );
        let candidates = vec![account("a", 3), account("b", 1)];
        let mut counts = HashMap::new();
        for _ in 0..4 {
            let picked = scheduler
                .select(ProviderKind::Mock, Region::Cn, &candidates)
                .await
                .unwrap();
            *counts.entry(picked.id).or_insert(0) += 1;
        }
        assert_eq!(counts.get("a"), Some(&3));
        assert_eq!(counts.get("b"), Some(&1));
    }

    #[tokio::test]
    async fn least_connections_prefers_the_idle_account() {
        let scheduler = Scheduler::new(
            SchedulerConfig {
                strategy: SchedulerStrategy::LeastConnections,
                ..SchedulerConfig::default()
            },
            None,
        );
        let candidates = vec![account("a", 1), account("b", 1)];
        scheduler.acquired("a");
        scheduler.acquired("a");
        let picked = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        assert_eq!(picked.id, "b");
    }

    async fn persisted_account(storage: &Storage, name: &str) -> Account {
        storage
            .accounts()
            .insert(&crate::accounts::NewAccount {
                provider: "mock-cn".into(),
                kind: ProviderKind::Mock,
                region: Some(Region::Cn),
                name: name.into(),
                credential: serde_json::json!({"token": "test-token"}),
                external_id: None,
                weight: 1,
                max_concurrency: 4,
                quota_group: None,
                enabled: true,
                metadata: serde_json::json!({}),
            })
            .await
            .expect("persist account")
    }

    #[tokio::test]
    async fn repeated_upstream_failures_cool_an_account_down() {
        let scheduler = Scheduler::new(
            SchedulerConfig {
                strategy: SchedulerStrategy::RoundRobin,
                failure_threshold: 2,
                cooldown_base_secs: 60,
                cooldown_max_secs: 60,
                ..SchedulerConfig::default()
            },
            None,
        );
        let candidates = vec![account("a", 1), account("b", 1)];
        scheduler.release("a", Some(ErrorKind::UpstreamServer));
        scheduler.release("a", Some(ErrorKind::Transport));
        assert!(scheduler.in_cooldown("a", Utc::now().timestamp()));
        let picked = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        assert_eq!(picked.id, "b");
    }

    #[tokio::test]
    async fn client_errors_never_cool_an_account_down() {
        let scheduler = Scheduler::new(
            SchedulerConfig {
                failure_threshold: 1,
                ..SchedulerConfig::default()
            },
            None,
        );
        scheduler.release("a", Some(ErrorKind::BadRequest));
        scheduler.release("a", Some(ErrorKind::QuotaExhausted));
        assert!(!scheduler.in_cooldown("a", Utc::now().timestamp()));
    }

    #[tokio::test]
    async fn known_exhausted_quota_excludes_an_account() {
        let storage = Storage::open_memory().await.unwrap();
        let a = persisted_account(&storage, "a").await;
        let mut quota = crate::quota::QuotaSnapshot::unknown(&a.id, "");
        quota.status = QuotaStatus::Known;
        quota.unit = crate::quota::QuotaUnit::Credits;
        quota.total = Some(100.0);
        quota.remaining = Some(0.0);
        quota.error = None;
        storage.quota().save(&quota).await.unwrap();

        let scheduler = Scheduler::new(SchedulerConfig::default(), Some(storage));
        let candidates = vec![a.clone(), account("b", 1)];
        let picked = scheduler
            .select(ProviderKind::Mock, Region::Cn, &candidates)
            .await
            .unwrap();
        assert_eq!(picked.id, "b");
    }

    #[tokio::test]
    async fn unknown_quota_stays_eligible() {
        let storage = Storage::open_memory().await.unwrap();
        let a = persisted_account(&storage, "a").await;
        let quota = crate::quota::QuotaSnapshot::unknown(&a.id, "no data");
        storage.quota().save(&quota).await.unwrap();
        let scheduler = Scheduler::new(SchedulerConfig::default(), Some(storage));
        let picked = scheduler
            .select(ProviderKind::Mock, Region::Cn, std::slice::from_ref(&a))
            .await
            .unwrap();
        assert_eq!(picked.id, a.id);
    }
}
