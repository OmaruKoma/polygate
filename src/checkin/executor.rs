//! Check-in execution boundary.
//!
//! M12 ships only a deterministic mock. The trait is deliberately separate from
//! the provider registry: a check-in is a side-effecting operation with its own
//! idempotency rules (status query, already-checked-in, unknown-after-timeout),
//! which a chat adapter does not model. M13/M14 decide how a real adapter
//! implements this trait; nothing here calls the network.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::account_service::AuthorizationContext;
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};

use super::{CheckinOutcome, CheckinRequest, CheckinStatus};

/// Answer to "did the upstream already record today's check-in?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckinProbe {
    /// The upstream answered definitively.
    Confirmed(CheckinStatus),
    /// The upstream cannot be queried (no endpoint, timeout, unsupported).
    /// Callers must treat this as "outcome unknown", never as "not done".
    Unknown,
}

impl CheckinProbe {
    pub fn confirmed(status: CheckinStatus) -> Self {
        CheckinProbe::Confirmed(status)
    }
}

#[async_trait]
pub trait CheckinExecutor: Send + Sync {
    /// Whether this executor claims check-in support for the provider kind.
    /// Returning `false` is what turns a manual or scheduled run into
    /// `Unsupported` without touching the account's credential.
    fn supports(&self, kind: ProviderKind) -> bool;

    /// Ask the upstream whether the business date is already checked in.
    /// `Err(Unsupported)` means the provider has no status endpoint at all.
    async fn check_status(
        &self,
        request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError>;

    /// Perform the check-in. A transport timeout must be returned as an error
    /// (mapped to `Unknown`), not as `Failed`: the request may already have been
    /// applied upstream.
    async fn checkin(
        &self,
        request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError>;
}

/// Dispatches each account to the executor that supports its provider kind.
/// Used by the runtime to register the mock and the CodeArts adapter together
/// without teaching the check-in service about either one.
#[derive(Default, Clone)]
pub struct RoutingCheckinExecutor {
    executors: HashMap<(ProviderKind, Region), Arc<dyn CheckinExecutor>>,
}

impl RoutingCheckinExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an executor for one provider kind *and* region: CN and Intl
    /// CodeArts endpoints are different upstreams with different credentials.
    pub fn with(
        mut self,
        kind: ProviderKind,
        region: Region,
        executor: Arc<dyn CheckinExecutor>,
    ) -> Self {
        self.executors.insert((kind, region), executor);
        self
    }

    fn for_request(&self, kind: ProviderKind, region: Region) -> Option<&Arc<dyn CheckinExecutor>> {
        self.executors.get(&(kind, region)).or_else(|| {
            self.executors
                .iter()
                .find(|((candidate, _), _)| *candidate == kind)
                .map(|(_, executor)| executor)
        })
    }
}

#[async_trait]
impl CheckinExecutor for RoutingCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        self.executors
            .iter()
            .filter(|((candidate, _), _)| *candidate == kind)
            .any(|(_, executor)| executor.supports(kind))
    }

    async fn check_status(
        &self,
        request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        let executor = self
            .for_request(request.kind, request.region)
            .ok_or_else(|| {
                GatewayError::unsupported(format!(
                    "no check-in executor is registered for {}-{}",
                    request.kind.as_str(),
                    request.region.as_str()
                ))
            })?;
        executor.check_status(request, authorization).await
    }

    async fn checkin(
        &self,
        request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let executor = self
            .for_request(request.kind, request.region)
            .ok_or_else(|| {
                GatewayError::unsupported(format!(
                    "no check-in executor is registered for {}-{}",
                    request.kind.as_str(),
                    request.region.as_str()
                ))
            })?;
        executor.checkin(request, authorization).await
    }
}

/// One scripted response for the mock executor.
#[derive(Debug, Clone)]
pub enum MockCheckinStep {
    Outcome(CheckinOutcome),
    /// Return a classified error from `checkin()`.
    Error(ErrorKind),
}

/// Deterministic in-memory executor used by M12 tests and the development
/// runtime. It never performs I/O and is safe to share across tasks.
#[derive(Clone)]
pub struct MockCheckinExecutor {
    state: Arc<MockState>,
}

struct MockState {
    supported: Mutex<Option<Vec<ProviderKind>>>,
    steps: Mutex<HashMap<String, VecDeque<MockCheckinStep>>>,
    default_step: Mutex<MockCheckinStep>,
    probes: Mutex<HashMap<String, VecDeque<CheckinProbe>>>,
    default_probe: Mutex<CheckinProbe>,
    checkin_calls: AtomicUsize,
    status_calls: AtomicUsize,
    last_request: Mutex<Option<CheckinRequest>>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            supported: Mutex::new(None),
            steps: Mutex::new(HashMap::new()),
            default_step: Mutex::new(MockCheckinStep::Outcome(CheckinOutcome::Succeeded {
                summary: "mock check-in succeeded".into(),
            })),
            probes: Mutex::new(HashMap::new()),
            default_probe: Mutex::new(CheckinProbe::Unknown),
            checkin_calls: AtomicUsize::new(0),
            status_calls: AtomicUsize::new(0),
            last_request: Mutex::new(None),
        }
    }
}

impl Default for MockCheckinExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl MockCheckinExecutor {
    /// Supports every provider kind.
    pub fn new() -> Self {
        Self {
            state: Arc::new(MockState::default()),
        }
    }

    /// Restrict support to the given kinds; every other kind reports
    /// `Unsupported`.
    pub fn support_only(kinds: impl IntoIterator<Item = ProviderKind>) -> Self {
        let executor = Self::new();
        *executor.state.supported.lock() = Some(kinds.into_iter().collect());
        executor
    }

    /// Queue steps for one account. Steps are consumed in order.
    pub fn script(&self, account_id: &str, steps: impl IntoIterator<Item = MockCheckinStep>) {
        self.state
            .steps
            .lock()
            .entry(account_id.to_string())
            .or_default()
            .extend(steps);
    }

    pub fn set_default_step(&self, step: MockCheckinStep) {
        *self.state.default_step.lock() = step;
    }

    pub fn script_status(&self, account_id: &str, probes: impl IntoIterator<Item = CheckinProbe>) {
        self.state
            .probes
            .lock()
            .entry(account_id.to_string())
            .or_default()
            .extend(probes);
    }

    pub fn set_default_probe(&self, probe: CheckinProbe) {
        *self.state.default_probe.lock() = probe;
    }

    pub fn checkin_calls(&self) -> usize {
        self.state.checkin_calls.load(Ordering::SeqCst)
    }

    pub fn status_calls(&self) -> usize {
        self.state.status_calls.load(Ordering::SeqCst)
    }

    pub fn last_request(&self) -> Option<CheckinRequest> {
        self.state.last_request.lock().clone()
    }
}

#[async_trait]
impl CheckinExecutor for MockCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        match self.state.supported.lock().as_ref() {
            Some(kinds) => kinds.contains(&kind),
            None => true,
        }
    }

    async fn check_status(
        &self,
        request: &CheckinRequest,
        _authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        self.state.status_calls.fetch_add(1, Ordering::SeqCst);
        let mut probes = self.state.probes.lock();
        if let Some(queue) = probes.get_mut(&request.account_id) {
            if let Some(probe) = queue.pop_front() {
                return Ok(probe);
            }
        }
        Ok(self.state.default_probe.lock().clone())
    }

    async fn checkin(
        &self,
        request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        self.state.checkin_calls.fetch_add(1, Ordering::SeqCst);
        *self.state.last_request.lock() = Some(request.clone());
        // The mock never reads the secret, but asserts that the caller supplied
        // a decrypted context only for its own account.
        debug_assert_eq!(authorization.account_id, request.account_id);

        let step = {
            let mut steps = self.state.steps.lock();
            steps
                .get_mut(&request.account_id)
                .and_then(|queue| queue.pop_front())
        }
        .unwrap_or_else(|| self.state.default_step.lock().clone());

        match step {
            MockCheckinStep::Outcome(outcome) => Ok(outcome),
            MockCheckinStep::Error(kind) => Err(GatewayError::new(kind, "mock check-in failure")),
        }
    }
}
