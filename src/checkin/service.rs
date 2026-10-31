//! Check-in service: manual runs, batches, scheduled runs and crash recovery.
//!
//! Ordering rules that matter:
//!
//! 1. a run is claimed in SQLite *before* the executor is called;
//! 2. the credential is decrypted only after the claim and after the provider is
//!    known to support check-in, and it is dropped as soon as the call returns;
//! 3. an ambiguous failure (timeout, transport, 5xx, local persistence failure)
//!    is stored as `Unknown`, never `Failed`, and is never retried without an
//!    upstream status probe;
//! 4. a successful check-in triggers a quota refresh whose failure is isolated.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;
use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use tracing::warn;

use crate::account_service::AccountService;
use crate::accounts::{Account, AuthStatus, Credential};
use crate::error::{redact_secrets, GatewayError};
use crate::storage::{RunClaim, Storage};

use super::executor::CheckinExecutor;
use super::{
    business_date_at, classify_error, default_timezone, due_business_date, parse_timezone,
    probe_failure_category, AuthRefreshHook, AuthRefreshRequest, CheckinErrorCategory,
    CheckinPolicy, CheckinProbe, CheckinRecord, CheckinRequest, CheckinResult, CheckinRetryMode,
    CheckinSchedule, CheckinStatus, Clock, NoopAuthRefresh, QuotaRefreshHook, QuotaRefreshOutcome,
    QuotaRefreshRequest, SystemClock, BATCH_CONCURRENCY, DEFAULT_LEASE_SECS, MAX_BATCH_ACCOUNTS,
};

const SUMMARY_MAX_CHARS: usize = 512;

#[derive(Clone)]
pub struct CheckinService {
    storage: Storage,
    accounts: AccountService,
    executor: Arc<dyn CheckinExecutor>,
    quota: Arc<dyn QuotaRefreshHook>,
    auth_refresh: Arc<dyn AuthRefreshHook>,
    clock: Arc<dyn Clock>,
    lease: Duration,
}

impl CheckinService {
    pub fn new(
        storage: Storage,
        accounts: AccountService,
        executor: Arc<dyn CheckinExecutor>,
        quota: Arc<dyn QuotaRefreshHook>,
    ) -> Self {
        Self {
            storage,
            accounts,
            executor,
            quota,
            auth_refresh: Arc::new(NoopAuthRefresh),
            clock: Arc::new(SystemClock),
            lease: Duration::seconds(DEFAULT_LEASE_SECS),
        }
    }

    /// Freshen an expiring credential before the executor runs. OfficeAce
    /// signs claims with a temporary AK/SK that lives two hours, so a daily run
    /// would otherwise present an expired `X-Security-Token`.
    pub fn with_auth_refresh(mut self, hook: Arc<dyn AuthRefreshHook>) -> Self {
        self.auth_refresh = hook;
        self
    }

    /// Replace the wall clock (tests use a fixed clock; business dates follow it
    /// through the provider time zone).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Override the ownership lease, mostly for recovery tests.
    pub fn with_lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    // ------------------------------------------------------------- manual runs

    /// Check in one account for the current business date in its provider time
    /// zone. Repeated calls on the same day return the stored record without
    /// calling the executor again.
    ///
    /// A locally disabled account is still allowed here: `enabled` controls
    /// dispatch, not credential maintenance. The automatic loop skips disabled
    /// accounts, and batch/all only target enabled ones.
    pub async fn checkin(&self, account_id: &str) -> Result<CheckinResult, GatewayError> {
        let account = self.account(account_id).await?;
        let (date, timezone) = self.account_day(&account).await?;
        self.run_for(&account, date, timezone, CheckinRetryMode::Manual)
            .await
    }

    /// Check in a list of accounts with bounded concurrency. One account's
    /// failure never aborts the others; every account gets its own result.
    pub async fn checkin_batch(
        &self,
        account_ids: Vec<String>,
    ) -> Result<Vec<CheckinResult>, GatewayError> {
        if account_ids.len() > MAX_BATCH_ACCOUNTS {
            return Err(GatewayError::bad_request(format!(
                "a check-in batch is limited to {MAX_BATCH_ACCOUNTS} accounts"
            )));
        }
        let mut seen = HashSet::new();
        let ids: Vec<String> = account_ids
            .into_iter()
            .filter_map(|id| {
                let id = id.trim().to_string();
                (!id.is_empty() && seen.insert(id.clone())).then_some(id)
            })
            .collect();

        let mut results: Vec<(usize, CheckinResult)> =
            stream::iter(ids.into_iter().enumerate().map(|(index, id)| {
                let service = self.clone();
                async move {
                    let result = match service.checkin(&id).await {
                        Ok(result) => result,
                        Err(error) => {
                            error.log("checkin_batch");
                            let date = business_date_at(
                                service.clock.now(),
                                parse_timezone("UTC").expect("UTC is a built-in zone"),
                            );
                            error_result(&id, date, &error)
                        }
                    };
                    (index, result)
                }
            }))
            .buffer_unordered(BATCH_CONCURRENCY)
            .collect()
            .await;
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    }

    /// One-click check-in for every account that could be dispatched: enabled,
    /// auth-dispatchable and holding a credential. Chunked so an account pool
    /// larger than [`MAX_BATCH_ACCOUNTS`] still works.
    pub async fn checkin_all(&self) -> Result<Vec<CheckinResult>, GatewayError> {
        let mut ids = Vec::new();
        for account in self.accounts.list().await? {
            if !account.enabled || !account.auth_status.is_dispatchable() {
                continue;
            }
            let present = self
                .accounts
                .credentials()
                .status(&account.id)
                .await?
                .is_some_and(|status| status.present);
            if present {
                ids.push(account.id);
            }
        }
        let mut results = Vec::new();
        for chunk in ids.chunks(MAX_BATCH_ACCOUNTS) {
            results.extend(self.checkin_batch(chunk.to_vec()).await?);
        }
        Ok(results)
    }

    // -------------------------------------------------------- schedules & auto

    /// Persist one account's schedule. The time zone is canonicalized through the
    /// IANA database before it is written.
    pub async fn set_schedule(
        &self,
        account_id: &str,
        policy: CheckinPolicy,
    ) -> Result<CheckinSchedule, GatewayError> {
        self.account(account_id).await?;
        policy.validate()?;
        let timezone = parse_timezone(&policy.timezone)?;
        let schedule = CheckinSchedule {
            account_id: account_id.to_string(),
            policy: CheckinPolicy {
                timezone: timezone.name().to_string(),
                ..policy
            },
        };
        self.storage
            .checkin()
            .upsert_schedule(&schedule, self.clock.now())
            .await?;
        Ok(schedule)
    }

    pub async fn schedule(
        &self,
        account_id: &str,
    ) -> Result<Option<CheckinSchedule>, GatewayError> {
        self.storage.checkin().get_schedule(account_id).await
    }

    pub async fn schedules(&self) -> Result<Vec<CheckinSchedule>, GatewayError> {
        self.storage.checkin().list_schedules().await
    }

    pub async fn set_schedule_enabled(
        &self,
        account_id: &str,
        enabled: bool,
    ) -> Result<bool, GatewayError> {
        self.storage
            .checkin()
            .set_schedule_enabled(account_id, enabled, self.clock.now())
            .await
    }

    pub async fn delete_schedule(&self, account_id: &str) -> Result<bool, GatewayError> {
        self.storage.checkin().delete_schedule(account_id).await
    }

    /// Execute every schedule that is currently inside its occurrence + catch-up
    /// window. Safe to call from multiple schedulers: the database claim decides
    /// who actually runs each account.
    pub async fn run_due(&self) -> Result<Vec<CheckinResult>, GatewayError> {
        let now = self.clock.now();
        let mut due: Vec<(Account, NaiveDate, Tz, u32)> = Vec::new();
        for schedule in self.storage.checkin().list_schedules().await? {
            if !schedule.policy.enabled {
                continue;
            }
            let timezone = match parse_timezone(&schedule.policy.timezone) {
                Ok(timezone) => timezone,
                Err(error) => {
                    error.log("checkin_schedule");
                    continue;
                }
            };
            let Some(account) = self.accounts.get(&schedule.account_id).await? else {
                continue;
            };
            if !account.enabled {
                continue;
            }
            let Some(date) = due_business_date(&account.id, &schedule.policy, timezone, now) else {
                continue;
            };
            if let Some(existing) = self.storage.checkin().get_run(&account.id, date).await? {
                if !existing.status.allows_auto_retry() {
                    continue;
                }
                let retry = CheckinRetryMode::Automatic {
                    max_attempts: schedule.policy.max_attempts,
                };
                if !retry.allows_failed(&existing) {
                    continue;
                }
            }
            due.push((account, date, timezone, schedule.policy.max_attempts));
        }

        let mut results: Vec<(usize, CheckinResult)> =
            stream::iter(due.into_iter().enumerate().map(
                |(index, (account, date, timezone, max_attempts))| {
                    let service = self.clone();
                    async move {
                        let id = account.id.clone();
                        let retry = CheckinRetryMode::Automatic { max_attempts };
                        let result = match service.run_for(&account, date, timezone, retry).await {
                            Ok(result) => result,
                            Err(error) => {
                                error.log("checkin_scheduler");
                                error_result(&id, date, &error)
                            }
                        };
                        (index, result)
                    }
                },
            ))
            .buffer_unordered(BATCH_CONCURRENCY)
            .collect()
            .await;
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    }

    // ------------------------------------------------------------- recovery

    /// Resolve runs whose worker died while holding the lease. The upstream
    /// status query is the only confirmation path; without one the run becomes
    /// `Unknown` and is never re-executed.
    pub async fn recover_interrupted(&self) -> Result<Vec<CheckinResult>, GatewayError> {
        let now = self.clock.now();
        let mut results = Vec::new();
        for record in self.storage.checkin().list_stale_running(now).await? {
            let Some(account) = self.accounts.get(&record.account_id).await? else {
                continue;
            };
            let timezone = parse_timezone(&record.timezone).unwrap_or_else(|_| {
                parse_timezone(default_timezone(record.region)).expect("built-in zone")
            });
            match self.resolve_interrupted(&account, record, timezone).await {
                Ok(result) => results.push(result),
                Err(error) => error.log("checkin_recovery"),
            }
        }
        Ok(results)
    }

    pub async fn history(
        &self,
        account_id: &str,
        limit: i64,
    ) -> Result<Vec<CheckinRecord>, GatewayError> {
        self.storage.checkin().list_runs(account_id, limit).await
    }

    // -------------------------------------------------------------- internals

    async fn account(&self, account_id: &str) -> Result<Account, GatewayError> {
        self.accounts
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::bad_request("account not found"))
    }

    async fn account_day(&self, account: &Account) -> Result<(NaiveDate, Tz), GatewayError> {
        let timezone = self.account_timezone(account).await?;
        Ok((business_date_at(self.clock.now(), timezone), timezone))
    }

    async fn account_timezone(&self, account: &Account) -> Result<Tz, GatewayError> {
        if let Some(schedule) = self.storage.checkin().get_schedule(&account.id).await? {
            match parse_timezone(&schedule.policy.timezone) {
                Ok(timezone) => return Ok(timezone),
                Err(_) => warn!(
                    account = %account.id,
                    "stored check-in time zone is invalid; using the provider default"
                ),
            }
        }
        parse_timezone(default_timezone(account.region))
    }

    async fn run_for(
        &self,
        account: &Account,
        date: NaiveDate,
        timezone: Tz,
        retry: CheckinRetryMode,
    ) -> Result<CheckinResult, GatewayError> {
        let claim = self
            .storage
            .checkin()
            .claim(
                &account.id,
                account.kind,
                account.region,
                date,
                timezone.name(),
                self.clock.now(),
                self.lease,
                retry,
            )
            .await?;
        match claim {
            RunClaim::Claimed(record) => self.execute(account, record, timezone).await,
            RunClaim::Busy(record) => Ok(record.to_result(true)),
            RunClaim::Existing(record) if record.status == CheckinStatus::Unknown => {
                self.reconcile_unknown(account, record, timezone).await
            }
            RunClaim::Existing(record) => Ok(record.to_result(true)),
            RunClaim::Interrupted(record) => {
                self.resolve_interrupted(account, record, timezone).await
            }
        }
    }

    async fn execute(
        &self,
        account: &Account,
        record: CheckinRecord,
        timezone: Tz,
    ) -> Result<CheckinResult, GatewayError> {
        let request = CheckinRequest {
            account_id: account.id.clone(),
            kind: account.kind,
            region: account.region,
            business_date: record.business_date,
            timezone: timezone.name().to_string(),
            attempt: record.attempt_count,
        };

        if !self.executor.supports(account.kind) {
            return self
                .finish(
                    record,
                    CheckinStatus::Unsupported,
                    Some(CheckinErrorCategory::Unsupported),
                    "provider does not support check-in",
                    None,
                )
                .await;
        }

        // Refresh before the write. A failure here is not fatal: the executor
        // still runs and reports the upstream error in its own category.
        if let Err(error) = self
            .auth_refresh
            .refresh(&AuthRefreshRequest {
                account_id: account.id.clone(),
                kind: account.kind,
                region: account.region,
            })
            .await
        {
            warn!(
                account = %account.id,
                provider = %account.provider,
                "credential refresh before check-in failed: {}",
                error.message
            );
        }
        let authorization = match self
            .accounts
            .authorization_context_for(&account.id, account.kind, account.region)
            .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                let category = CheckinErrorCategory::from_error_kind(error.kind);
                return self
                    .finish(
                        record,
                        CheckinStatus::Failed,
                        Some(category),
                        &format!("cannot build authorization context: {}", error.message),
                        None,
                    )
                    .await;
            }
        };
        if authorization.credential.is_none() {
            return self
                .finish(
                    record,
                    CheckinStatus::Failed,
                    Some(CheckinErrorCategory::Auth),
                    "account has no stored credential",
                    None,
                )
                .await;
        }

        let outcome = self.executor.checkin(&request, &authorization).await;
        let mut authorization = authorization;
        let secret = authorization.credential.take();
        drop(authorization);

        match outcome {
            Ok(outcome) => {
                let status = outcome.status();
                let category = outcome.category();
                let summary = outcome.summary().to_string();
                self.finish(record, status, category, &summary, secret.as_ref())
                    .await
            }
            Err(error) => {
                let (status, category) = classify_error(&error);
                if error.kind.blames_account() {
                    let _ = self.accounts.mark_auth_failure(&account.id, &error).await;
                }
                let summary = error.message.clone();
                self.finish(record, status, Some(category), &summary, secret.as_ref())
                    .await
            }
        }
    }

    async fn resolve_interrupted(
        &self,
        account: &Account,
        record: CheckinRecord,
        timezone: Tz,
    ) -> Result<CheckinResult, GatewayError> {
        let request = CheckinRequest {
            account_id: account.id.clone(),
            kind: account.kind,
            region: account.region,
            business_date: record.business_date,
            timezone: timezone.name().to_string(),
            attempt: record.attempt_count,
        };

        if !self.executor.supports(account.kind) {
            return self
                .finish(
                    record,
                    CheckinStatus::Unknown,
                    Some(CheckinErrorCategory::Unsupported),
                    "interrupted run; provider cannot confirm the outcome",
                    None,
                )
                .await;
        }

        let authorization = match self
            .accounts
            .authorization_context_for(&account.id, account.kind, account.region)
            .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                let category = CheckinErrorCategory::from_error_kind(error.kind);
                return self
                    .finish(
                        record,
                        CheckinStatus::Unknown,
                        Some(category),
                        &format!(
                            "interrupted run; no authorization to confirm: {}",
                            error.message
                        ),
                        None,
                    )
                    .await;
            }
        };
        let probe = self.executor.check_status(&request, &authorization).await;
        let mut authorization = authorization;
        let secret = authorization.credential.take();
        drop(authorization);

        let (status, category, summary) = match probe {
            Ok(CheckinProbe::Confirmed(status)) => match status {
                CheckinStatus::Succeeded => (
                    CheckinStatus::Succeeded,
                    None,
                    "upstream confirmed the check-in succeeded after an interrupted run",
                ),
                CheckinStatus::AlreadyCheckedIn => (
                    CheckinStatus::AlreadyCheckedIn,
                    None,
                    "upstream reports the day as already checked in",
                ),
                CheckinStatus::Failed => (
                    CheckinStatus::Failed,
                    Some(probe_failure_category()),
                    "upstream reports the check-in failed",
                ),
                CheckinStatus::Unsupported => (
                    CheckinStatus::Unsupported,
                    Some(CheckinErrorCategory::Unsupported),
                    "provider does not support check-in",
                ),
                _ => (
                    CheckinStatus::Unknown,
                    Some(CheckinErrorCategory::Other),
                    "status query did not confirm the outcome",
                ),
            },
            Ok(CheckinProbe::Unknown) | Err(_) => (
                CheckinStatus::Unknown,
                Some(CheckinErrorCategory::Other),
                "interrupted while running; outcome unconfirmed",
            ),
        };
        self.finish(record, status, category, summary, secret.as_ref())
            .await
    }

    /// A repeat call for an `unknown` day may query the upstream status, but it
    /// never re-executes the check-in. Only a confirmed success (or already-done)
    /// updates the stored row; everything else leaves it `unknown`.
    async fn reconcile_unknown(
        &self,
        account: &Account,
        record: CheckinRecord,
        timezone: Tz,
    ) -> Result<CheckinResult, GatewayError> {
        if !self.executor.supports(account.kind) {
            return Ok(record.to_result(true));
        }
        let authorization = match self
            .accounts
            .authorization_context_for(&account.id, account.kind, account.region)
            .await
        {
            Ok(authorization) => authorization,
            Err(_) => return Ok(record.to_result(true)),
        };
        let request = CheckinRequest {
            account_id: account.id.clone(),
            kind: account.kind,
            region: account.region,
            business_date: record.business_date,
            timezone: timezone.name().to_string(),
            attempt: record.attempt_count,
        };
        let probe = self.executor.check_status(&request, &authorization).await;
        let mut authorization = authorization;
        let secret = authorization.credential.take();
        drop(authorization);

        let (status, summary) = match probe {
            Ok(CheckinProbe::Confirmed(CheckinStatus::Succeeded)) => (
                CheckinStatus::Succeeded,
                "upstream confirmed the check-in succeeded",
            ),
            Ok(CheckinProbe::Confirmed(CheckinStatus::AlreadyCheckedIn)) => (
                CheckinStatus::AlreadyCheckedIn,
                "upstream reports the day as already checked in",
            ),
            _ => return Ok(record.to_result(true)),
        };
        let summary = sanitize_summary(summary, secret.as_ref());
        let updated = self
            .storage
            .checkin()
            .confirm_unknown(&record.id, status, &summary, self.clock.now())
            .await?;
        if !updated {
            if let Some(current) = self
                .storage
                .checkin()
                .get_run(&record.account_id, record.business_date)
                .await?
            {
                return Ok(current.to_result(true));
            }
            return Err(GatewayError::internal("check-in run disappeared"));
        }
        let _ = self
            .accounts
            .record_auth_status(&record.account_id, AuthStatus::Valid, None)
            .await;
        let quota_refresh = Some(self.refresh_quota(&record).await);
        Ok(CheckinResult {
            account_id: record.account_id,
            business_date: record.business_date,
            status,
            replayed: true,
            attempt_count: record.attempt_count,
            error_category: None,
            summary,
            quota_refresh,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        record: CheckinRecord,
        status: CheckinStatus,
        category: Option<CheckinErrorCategory>,
        summary: &str,
        secret: Option<&Credential>,
    ) -> Result<CheckinResult, GatewayError> {
        let summary = sanitize_summary(summary, secret);
        let completed = self
            .storage
            .checkin()
            .complete(&record.id, status, category, &summary, self.clock.now())
            .await?;
        if !completed {
            // Someone else completed (or recovered) the run first. Report what is
            // actually stored instead of pretending this call won.
            if let Some(current) = self
                .storage
                .checkin()
                .get_run(&record.account_id, record.business_date)
                .await?
            {
                return Ok(current.to_result(true));
            }
            return Err(GatewayError::internal("check-in run disappeared"));
        }

        let mut quota_refresh = None;
        if status.is_success() {
            // A successful authenticated call is usable evidence for the account.
            let _ = self
                .accounts
                .record_auth_status(&record.account_id, AuthStatus::Valid, None)
                .await;
            quota_refresh = Some(self.refresh_quota(&record).await);
        }

        Ok(CheckinResult {
            account_id: record.account_id,
            business_date: record.business_date,
            status,
            replayed: false,
            attempt_count: record.attempt_count,
            error_category: category,
            summary,
            quota_refresh,
        })
    }

    async fn refresh_quota(&self, record: &CheckinRecord) -> QuotaRefreshOutcome {
        let request = QuotaRefreshRequest {
            account_id: record.account_id.clone(),
            kind: record.kind,
            region: record.region,
            business_date: record.business_date,
        };
        match self.quota.refresh(&request).await {
            Ok(()) => QuotaRefreshOutcome {
                attempted: true,
                ok: true,
                message: "quota refresh requested".into(),
            },
            Err(error) => {
                error.log("checkin_quota_refresh");
                QuotaRefreshOutcome {
                    attempted: true,
                    ok: false,
                    message: redact_secrets(&error.message),
                }
            }
        }
    }
}

fn error_result(account_id: &str, business_date: NaiveDate, error: &GatewayError) -> CheckinResult {
    let (status, category) = classify_error(error);
    CheckinResult {
        account_id: account_id.to_string(),
        business_date,
        status,
        replayed: false,
        attempt_count: 0,
        error_category: Some(category),
        summary: sanitize_summary(&error.message, None),
        quota_refresh: None,
    }
}

/// Redact key/value secrets, scrub any credential value echoed back by the
/// upstream, and cap the stored summary.
fn sanitize_summary(summary: &str, secret: Option<&Credential>) -> String {
    let mut text = summary.to_string();
    if let Some(credential) = secret {
        scrub_json_strings(credential.to_json(), &mut text);
    }
    let text = redact_secrets(&text);
    truncate(&text, SUMMARY_MAX_CHARS)
}

fn scrub_json_strings(value: &Value, text: &mut String) {
    match value {
        Value::String(inner) if inner.len() >= 4 => {
            if text.contains(inner.as_str()) {
                *text = text.replace(inner.as_str(), "***");
            }
        }
        Value::Array(items) => items.iter().for_each(|item| scrub_json_strings(item, text)),
        Value::Object(map) => map.values().for_each(|item| scrub_json_strings(item, text)),
        _ => {}
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}
