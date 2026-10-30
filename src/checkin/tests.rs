//! Service-level tests for M12.
//!
//! All time comes from [`TestClock`]; no test sleeps waiting for a wall clock and
//! no test touches a real upstream. The only file-backed stores are the restart
//! and crash-recovery cases, which need a real SQLite file to reopen.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::account_service::{AccountService, CreateAccount};
use crate::accounts::{Account, AuthStatus};
use crate::checkin::{
    business_date_at, parse_timezone, CheckinErrorCategory, CheckinExecutor, CheckinOutcome,
    CheckinPolicy, CheckinProbe, CheckinRetryMode, CheckinScheduler, CheckinService, CheckinStatus,
    Clock, MockCheckinExecutor, MockCheckinStep, MockQuotaRefreshHook, Sleeper,
};
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::security::SecretBox;
use crate::storage::{RunClaim, Storage};

struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    fn new(now: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now),
        })
    }

    fn set(&self, now: DateTime<Utc>) {
        *self.now.lock() = now;
    }

    fn advance(&self, by: Duration) {
        let mut now = self.now.lock();
        *now += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock()
    }
}

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("valid instant")
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

fn secret_box() -> SecretBox {
    SecretBox::from_master_key(&[9u8; 32]).expect("32-byte key")
}

fn policy(timezone: &str, local_time: &str) -> CheckinPolicy {
    CheckinPolicy {
        enabled: true,
        timezone: timezone.into(),
        local_time: local_time.into(),
        jitter_secs: 0,
        catch_up_secs: 3600,
        max_attempts: 2,
    }
}

struct Fixture {
    _dir: Option<tempfile::TempDir>,
    path: Option<PathBuf>,
    storage: Storage,
    accounts: AccountService,
    executor: Arc<MockCheckinExecutor>,
    quota: Arc<MockQuotaRefreshHook>,
    clock: Arc<TestClock>,
    service: CheckinService,
}

impl Fixture {
    async fn memory() -> Self {
        Self::memory_with(Arc::new(MockCheckinExecutor::new())).await
    }

    async fn memory_with(executor: Arc<MockCheckinExecutor>) -> Self {
        let storage = Storage::open_memory().await.expect("memory db");
        let clock = TestClock::new(at(2026, 10, 29, 3, 0));
        let quota = Arc::new(MockQuotaRefreshHook::new());
        let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
        let service = CheckinService::new(
            storage.clone(),
            accounts.clone(),
            executor.clone(),
            quota.clone(),
        )
        .with_clock(clock.clone());
        Self {
            _dir: None,
            path: None,
            storage,
            accounts,
            executor,
            quota,
            clock,
            service,
        }
    }

    async fn file() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("polygate.db");
        let storage = Storage::open_file(&path, 4).await.expect("file db");
        let clock = TestClock::new(at(2026, 10, 29, 3, 0));
        let quota = Arc::new(MockQuotaRefreshHook::new());
        let executor = Arc::new(MockCheckinExecutor::new());
        let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
        let service = CheckinService::new(
            storage.clone(),
            accounts.clone(),
            executor.clone(),
            quota.clone(),
        )
        .with_clock(clock.clone());
        Self {
            _dir: Some(dir),
            path: Some(path),
            storage,
            accounts,
            executor,
            quota,
            clock,
            service,
        }
    }

    async fn create(&self, kind: ProviderKind, region: Region, name: &str) -> Account {
        let credential = serde_json::json!({
            "token": format!("token-{name}"),
            "cookie": format!("session-cookie-{name}"),
        });
        self.accounts
            .create(CreateAccount::new(kind, region, name, Some(credential)))
            .await
            .expect("create account")
    }

    async fn create_without_credential(&self, name: &str) -> Account {
        self.accounts
            .create(CreateAccount::new(
                ProviderKind::Codearts,
                Region::Cn,
                name,
                None,
            ))
            .await
            .expect("create account")
    }

    async fn set_schedule(&self, account: &Account, policy: CheckinPolicy) {
        self.service
            .set_schedule(&account.id, policy)
            .await
            .expect("schedule");
    }
}

// ---------------------------------------------------------------------------
// 1-4: manual check-in, independence, batch, isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_account_checkin_persists_record() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Succeeded);
    assert!(!result.replayed);
    assert_eq!(result.attempt_count, 1);
    assert_eq!(result.business_date, date(2026, 10, 29));
    assert_eq!(fixture.executor.checkin_calls(), 1);

    let history = fixture
        .service
        .history(&account.id, 10)
        .await
        .expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, CheckinStatus::Succeeded);
    assert_eq!(history[0].business_date, date(2026, 10, 29));
    assert!(history[0].started_at.is_some());
    assert!(history[0].completed_at.is_some());
}

#[tokio::test]
async fn two_accounts_check_in_independently() {
    let fixture = Fixture::memory().await;
    let alpha = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let beta = fixture
        .create(ProviderKind::Codearts, Region::Cn, "beta")
        .await;

    let first = fixture.service.checkin(&alpha.id).await.expect("alpha");
    let second = fixture.service.checkin(&beta.id).await.expect("beta");
    assert_eq!(first.status, CheckinStatus::Succeeded);
    assert_eq!(second.status, CheckinStatus::Succeeded);
    assert_ne!(first.account_id, second.account_id);
    assert_eq!(fixture.executor.checkin_calls(), 2);

    let records = fixture.service.history(&alpha.id, 10).await.expect("a");
    assert_eq!(records.len(), 1);
    let records = fixture.service.history(&beta.id, 10).await.expect("b");
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn batch_returns_one_result_per_account_in_order() {
    let fixture = Fixture::memory().await;
    let alpha = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let beta = fixture
        .create(ProviderKind::Codearts, Region::Cn, "beta")
        .await;
    let gamma = fixture
        .create(ProviderKind::Codearts, Region::Cn, "gamma")
        .await;
    fixture.executor.script(
        &beta.id,
        [MockCheckinStep::Outcome(CheckinOutcome::AlreadyCheckedIn {
            summary: "already claimed".into(),
        })],
    );

    let results = fixture
        .service
        .checkin_batch(vec![alpha.id.clone(), beta.id.clone(), gamma.id.clone()])
        .await
        .expect("batch");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].account_id, alpha.id);
    assert_eq!(results[0].status, CheckinStatus::Succeeded);
    assert_eq!(results[1].account_id, beta.id);
    assert_eq!(results[1].status, CheckinStatus::AlreadyCheckedIn);
    assert_eq!(results[2].account_id, gamma.id);
    assert_eq!(results[2].status, CheckinStatus::Succeeded);
    assert_eq!(fixture.executor.checkin_calls(), 3);
}

#[tokio::test]
async fn batch_isolates_one_failed_account() {
    let fixture = Fixture::memory().await;
    let alpha = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let beta = fixture
        .create(ProviderKind::Codearts, Region::Cn, "beta")
        .await;
    let gamma = fixture
        .create(ProviderKind::Codearts, Region::Cn, "gamma")
        .await;
    fixture
        .executor
        .script(&beta.id, [MockCheckinStep::Error(ErrorKind::AuthInvalid)]);

    let results = fixture
        .service
        .checkin_batch(vec![alpha.id.clone(), beta.id.clone(), gamma.id.clone()])
        .await
        .expect("batch");
    assert_eq!(results[0].status, CheckinStatus::Succeeded);
    assert_eq!(results[1].status, CheckinStatus::Failed);
    assert_eq!(results[1].error_category, Some(CheckinErrorCategory::Auth));
    assert_eq!(results[2].status, CheckinStatus::Succeeded);

    // The auth failure is recorded on the account, not on its neighbours.
    assert_eq!(
        fixture
            .accounts
            .auth_status(&beta.id)
            .await
            .expect("status"),
        Some(AuthStatus::Invalid)
    );
    assert_eq!(
        fixture
            .accounts
            .auth_status(&alpha.id)
            .await
            .expect("status"),
        Some(AuthStatus::Valid)
    );
}

#[tokio::test]
async fn batch_is_capped_at_two_hundred() {
    let fixture = Fixture::memory().await;
    let ids = (0..201).map(|i| format!("account-{i}")).collect();
    let error = fixture
        .service
        .checkin_batch(ids)
        .await
        .expect_err("over the cap");
    assert_eq!(error.kind, ErrorKind::BadRequest);
}

// ---------------------------------------------------------------------------
// 5: unsupported provider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsupported_provider_is_explicit_and_does_not_touch_credentials() {
    let fixture = Fixture::memory_with(Arc::new(MockCheckinExecutor::support_only([
        ProviderKind::Mock,
    ])))
    .await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Unsupported);
    assert_eq!(
        result.error_category,
        Some(CheckinErrorCategory::Unsupported)
    );
    assert_eq!(fixture.executor.checkin_calls(), 0);
    assert_eq!(fixture.executor.status_calls(), 0);
    // The credential is untouched and still decryptable.
    assert!(fixture
        .accounts
        .credentials()
        .load(&account.id)
        .await
        .expect("load")
        .is_some());
}

// ---------------------------------------------------------------------------
// 6-8: same-day dedupe, next day, time zone boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_day_checkin_is_deduped() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;

    let first = fixture.service.checkin(&account.id).await.expect("first");
    let second = fixture.service.checkin(&account.id).await.expect("second");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(second.status, first.status);
    assert_eq!(second.attempt_count, first.attempt_count);
    assert_eq!(fixture.executor.checkin_calls(), 1);

    // A duplicate inside one batch is collapsed before execution too.
    let results = fixture
        .service
        .checkin_batch(vec![account.id.clone(), account.id.clone()])
        .await
        .expect("batch");
    assert_eq!(results.len(), 1);
    assert_eq!(fixture.executor.checkin_calls(), 1);
}

#[tokio::test]
async fn next_business_date_runs_again() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;

    fixture.service.checkin(&account.id).await.expect("day one");
    fixture.clock.advance(Duration::days(1));
    let second = fixture.service.checkin(&account.id).await.expect("day two");
    assert!(!second.replayed);
    assert_eq!(second.business_date, date(2026, 10, 30));
    assert_eq!(fixture.executor.checkin_calls(), 2);
    assert_eq!(
        fixture
            .service
            .history(&account.id, 10)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn business_date_follows_the_schedule_time_zone() {
    let fixture = Fixture::memory().await;
    fixture.clock.set(at(2026, 10, 29, 16, 30));
    let cn = fixture
        .create(ProviderKind::Codearts, Region::Cn, "cn")
        .await;
    let intl = fixture
        .create(ProviderKind::Codearts, Region::Intl, "intl")
        .await;
    // Deliberately not the provider defaults: the schedule decides.
    fixture.set_schedule(&cn, policy("UTC", "00:05")).await;
    fixture
        .set_schedule(&intl, policy("Asia/Tokyo", "00:05"))
        .await;

    let cn_result = fixture.service.checkin(&cn.id).await.expect("cn");
    let intl_result = fixture.service.checkin(&intl.id).await.expect("intl");
    assert_eq!(cn_result.business_date, date(2026, 10, 29));
    assert_eq!(intl_result.business_date, date(2026, 10, 30));
}

// ---------------------------------------------------------------------------
// 9-13: concurrency, restart, recovery, Unknown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_workers_execute_at_most_once() {
    let fixture = Fixture::file().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;

    let service_a = fixture.service.clone();
    let service_b = fixture.service.clone();
    let id_a = account.id.clone();
    let id_b = account.id.clone();
    let (first, second) = tokio::join!(async move { service_a.checkin(&id_a).await }, async move {
        service_b.checkin(&id_b).await
    });
    let first = first.expect("worker a");
    let second = second.expect("worker b");

    assert_eq!(fixture.executor.checkin_calls(), 1);
    let results = [&first, &second];
    let executed = results.iter().filter(|result| !result.replayed).count();
    assert_eq!(executed, 1);
    // The winner finishes as Succeeded. The loser either observes the finished
    // row (Succeeded) or the live lease (Running); both mean "not re-executed".
    for result in results {
        assert!(
            matches!(
                result.status,
                CheckinStatus::Succeeded | CheckinStatus::Running
            ),
            "unexpected concurrent status {:?}",
            result.status
        );
    }
}

#[tokio::test]
async fn runs_and_schedules_survive_a_restart() {
    let fixture = Fixture::file().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture
        .set_schedule(&account, policy("Asia/Shanghai", "09:30"))
        .await;
    fixture.service.checkin(&account.id).await.expect("checkin");
    let path = fixture.path.clone().expect("file path");
    fixture.storage.close().await;

    let reopened = Storage::open_file(&path, 4).await.expect("reopen");
    let records = reopened
        .checkin()
        .list_runs(&account.id, 10)
        .await
        .expect("runs");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, CheckinStatus::Succeeded);
    let schedule = reopened
        .checkin()
        .get_schedule(&account.id)
        .await
        .expect("schedule")
        .expect("present");
    assert_eq!(schedule.policy.local_time, "09:30");
    assert!(schedule.policy.enabled);

    // Triggering the same business date again after the restart must replay the
    // stored row instead of calling the executor a second time.
    let clock = TestClock::new(at(2026, 10, 29, 3, 0));
    let executor = Arc::new(MockCheckinExecutor::new());
    let accounts = AccountService::new(reopened.clone(), Arc::new(secret_box()));
    let service = CheckinService::new(
        reopened.clone(),
        accounts,
        executor.clone(),
        Arc::new(MockQuotaRefreshHook::new()),
    )
    .with_clock(clock);
    let result = service
        .checkin(&account.id)
        .await
        .expect("dedupe after restart");
    assert!(result.replayed);
    assert_eq!(result.status, CheckinStatus::Succeeded);
    assert_eq!(executor.checkin_calls(), 0);
}

#[tokio::test]
async fn pending_run_is_claimed_after_restart() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let timezone = parse_timezone("Asia/Shanghai").expect("tz");
    let business_date = business_date_at(fixture.clock.now(), timezone);
    let pending = fixture
        .storage
        .checkin()
        .insert_pending(
            &account.id,
            account.kind,
            account.region,
            business_date,
            "Asia/Shanghai",
            fixture.clock.now(),
        )
        .await
        .expect("insert")
        .expect("created");
    assert_eq!(pending.status, CheckinStatus::Pending);

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Succeeded);
    assert!(!result.replayed);
    assert_eq!(fixture.executor.checkin_calls(), 1);
}

#[tokio::test]
async fn interrupted_run_is_confirmed_by_status_query_not_re_executed() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let timezone = parse_timezone("Asia/Shanghai").expect("tz");
    let business_date = business_date_at(fixture.clock.now(), timezone);
    fixture
        .storage
        .checkin()
        .insert_pending(
            &account.id,
            account.kind,
            account.region,
            business_date,
            "Asia/Shanghai",
            fixture.clock.now(),
        )
        .await
        .expect("insert")
        .expect("created");
    let claim = fixture
        .storage
        .checkin()
        .claim(
            &account.id,
            account.kind,
            account.region,
            business_date,
            "Asia/Shanghai",
            fixture.clock.now(),
            Duration::minutes(10),
            CheckinRetryMode::Manual,
        )
        .await
        .expect("claim");
    assert!(matches!(claim, RunClaim::Claimed(_)));

    fixture.executor.script_status(
        &account.id,
        [CheckinProbe::Confirmed(CheckinStatus::Succeeded)],
    );
    // The lease expires while the process is gone.
    fixture.clock.advance(Duration::minutes(11));

    let result = fixture.service.checkin(&account.id).await.expect("recover");
    assert_eq!(result.status, CheckinStatus::Succeeded);
    assert_eq!(result.attempt_count, 1);
    assert_eq!(fixture.executor.checkin_calls(), 0);
    assert_eq!(fixture.executor.status_calls(), 1);
}

#[tokio::test]
async fn unknown_outcome_is_never_retried_blindly() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture
        .executor
        .script(&account.id, [MockCheckinStep::Error(ErrorKind::Timeout)]);

    let first = fixture.service.checkin(&account.id).await.expect("first");
    assert_eq!(first.status, CheckinStatus::Unknown);
    assert_eq!(first.error_category, Some(CheckinErrorCategory::Timeout));

    let second = fixture.service.checkin(&account.id).await.expect("second");
    assert!(second.replayed);
    assert_eq!(second.status, CheckinStatus::Unknown);
    assert_eq!(fixture.executor.checkin_calls(), 1);
    // A repeat asks the upstream status once; it never re-executes the check-in.
    assert_eq!(fixture.executor.status_calls(), 1);

    // When the status query can confirm the outcome, the stored row is resolved
    // without a second check-in call.
    fixture.executor.script_status(
        &account.id,
        [CheckinProbe::Confirmed(CheckinStatus::Succeeded)],
    );
    let third = fixture.service.checkin(&account.id).await.expect("third");
    assert_eq!(third.status, CheckinStatus::Succeeded);
    assert!(third.replayed);
    assert_eq!(fixture.executor.checkin_calls(), 1);
    assert_eq!(fixture.executor.status_calls(), 2);
    assert_eq!(fixture.quota.calls(), 1);

    // The automatic loop must skip a resolved day as well.
    fixture
        .set_schedule(&account, policy("Asia/Shanghai", "03:00"))
        .await;
    let results = fixture.service.run_due().await.expect("due");
    assert!(results.is_empty());
    assert_eq!(fixture.executor.checkin_calls(), 1);
}

#[tokio::test]
async fn an_unsupported_day_is_re_run_only_on_manual_retry() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture.executor.script(
        &account.id,
        [MockCheckinStep::Outcome(CheckinOutcome::Unsupported)],
    );
    let first = fixture.service.checkin(&account.id).await.expect("first");
    assert_eq!(first.status, CheckinStatus::Unsupported);

    // Nothing was claimed upstream, so the automatic loop still leaves the day
    // alone, but an operator click may probe again after fixing the cause.
    let timezone = parse_timezone("Asia/Shanghai").expect("tz");
    let business_date = business_date_at(fixture.clock.now(), timezone);
    let automatic = fixture
        .storage
        .checkin()
        .claim(
            &account.id,
            account.kind,
            account.region,
            business_date,
            "Asia/Shanghai",
            fixture.clock.now(),
            Duration::minutes(10),
            CheckinRetryMode::Automatic { max_attempts: 3 },
        )
        .await
        .expect("claim");
    assert!(matches!(automatic, RunClaim::Existing(_)));

    let second = fixture.service.checkin(&account.id).await.expect("second");
    assert!(!second.replayed);
    assert_eq!(second.status, CheckinStatus::Succeeded);
    assert_eq!(fixture.executor.checkin_calls(), 2);
}

// ---------------------------------------------------------------------------
// 14-16: already checked in, quota refresh, refresh failure isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn already_checked_in_is_success_and_refreshes_quota() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture.executor.script(
        &account.id,
        [MockCheckinStep::Outcome(CheckinOutcome::AlreadyCheckedIn {
            summary: "already signed".into(),
        })],
    );

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::AlreadyCheckedIn);
    assert_eq!(fixture.quota.calls(), 1);
    let refresh = result.quota_refresh.expect("refresh outcome");
    assert!(refresh.attempted && refresh.ok);
    assert_eq!(
        fixture
            .accounts
            .auth_status(&account.id)
            .await
            .expect("auth"),
        Some(AuthStatus::Valid)
    );
}

#[tokio::test]
async fn successful_checkin_requests_quota_refresh() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Succeeded);
    assert_eq!(fixture.quota.calls(), 1);
    assert!(result.quota_refresh.expect("refresh").ok);
}

#[tokio::test]
async fn quota_refresh_failure_does_not_roll_back_success() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture.quota.fail(true);

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Succeeded);
    let refresh = result.quota_refresh.expect("refresh outcome");
    assert!(refresh.attempted);
    assert!(!refresh.ok);
    let stored = fixture
        .service
        .history(&account.id, 1)
        .await
        .expect("history");
    assert_eq!(stored[0].status, CheckinStatus::Succeeded);
    assert!(stored[0].error_category.is_none());
}

// ---------------------------------------------------------------------------
// 17-18: automatic schedule and catch-up window
// ---------------------------------------------------------------------------

#[tokio::test]
async fn automatic_loop_runs_due_accounts_once() {
    let fixture = Fixture::memory().await;
    fixture.clock.set(at(2026, 10, 29, 3, 0));
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture.set_schedule(&account, policy("UTC", "03:00")).await;

    let first = fixture.service.run_due().await.expect("first tick");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, CheckinStatus::Succeeded);
    let second = fixture.service.run_due().await.expect("second tick");
    assert!(second.is_empty());
    assert_eq!(fixture.executor.checkin_calls(), 1);

    // The next day's occurrence is due again.
    fixture.clock.advance(Duration::days(1));
    let third = fixture.service.run_due().await.expect("third tick");
    assert_eq!(third.len(), 1);
    assert_eq!(fixture.executor.checkin_calls(), 2);

    // Disabling the schedule stops the loop without deleting the run history.
    assert!(fixture
        .service
        .set_schedule_enabled(&account.id, false)
        .await
        .expect("disable"));
    fixture.clock.advance(Duration::days(1));
    let fourth = fixture.service.run_due().await.expect("fourth tick");
    assert!(fourth.is_empty());
    assert_eq!(fixture.executor.checkin_calls(), 2);
}

#[tokio::test]
async fn catch_up_window_limits_late_runs() {
    let fixture = Fixture::memory().await;
    fixture.clock.set(at(2026, 10, 29, 4, 30));
    let inside = fixture
        .create(ProviderKind::Codearts, Region::Cn, "inside")
        .await;
    let outside = fixture
        .create(ProviderKind::Codearts, Region::Cn, "outside")
        .await;

    let mut wide = policy("UTC", "03:00");
    wide.catch_up_secs = 7200;
    fixture.set_schedule(&inside, wide).await;
    let mut narrow = policy("UTC", "03:00");
    narrow.catch_up_secs = 600;
    fixture.set_schedule(&outside, narrow).await;

    let results = fixture.service.run_due().await.expect("tick");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].account_id, inside.id);
    assert_eq!(fixture.executor.checkin_calls(), 1);
}

// ---------------------------------------------------------------------------
// 19: graceful shutdown
// ---------------------------------------------------------------------------

struct ManualSleeper {
    sleeps: AtomicUsize,
    tick: Arc<Notify>,
}

#[async_trait]
impl Sleeper for ManualSleeper {
    async fn sleep(&self, _duration: StdDuration) {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        self.tick.notify_waiters();
    }
}

#[tokio::test]
async fn scheduler_stops_promptly_on_shutdown_signal() {
    let fixture = Fixture::memory().await;
    let sleeper = Arc::new(ManualSleeper {
        sleeps: AtomicUsize::new(0),
        tick: Arc::new(Notify::new()),
    });
    let scheduler = CheckinScheduler::new(fixture.service.clone())
        .with_interval(StdDuration::from_secs(1))
        .with_sleeper(sleeper.clone());

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let run = scheduler.run(async move {
        let mut rx = stop_rx;
        let _ = rx.wait_for(|stop| *stop).await;
    });
    let handle = tokio::spawn(run);

    while sleeper.sleeps.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    stop_tx.send(true).expect("signal");
    tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect("scheduler stopped in time")
        .expect("scheduler task");
    assert!(sleeper.sleeps.load(Ordering::SeqCst) >= 1);
}

// ---------------------------------------------------------------------------
// 20: no secrets in records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn summaries_never_store_secrets() {
    let fixture = Fixture::memory().await;
    let account = fixture
        .create(ProviderKind::Codearts, Region::Cn, "alpha")
        .await;
    fixture.executor.script(
        &account.id,
        [MockCheckinStep::Outcome(CheckinOutcome::Succeeded {
            summary: "done token-token-alpha cookie=session-cookie-alpha".into(),
        })],
    );

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    let history = fixture
        .service
        .history(&account.id, 1)
        .await
        .expect("history");
    for text in [&result.summary, &history[0].summary] {
        assert!(!text.contains("token-alpha"), "leaked token in {text}");
        assert!(
            !text.contains("session-cookie-alpha"),
            "leaked cookie in {text}"
        );
        assert!(text.contains("***"), "secret should be masked: {text}");
    }
    let debug = format!("{result:?}");
    assert!(!debug.contains("token-alpha"));
    assert!(!debug.contains("session-cookie-alpha"));
}

// ---------------------------------------------------------------------------
// Extra: account without credential, upstream success + local save failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn account_without_credential_fails_without_calling_upstream() {
    let fixture = Fixture::memory().await;
    let account = fixture.create_without_credential("empty").await;

    let result = fixture.service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Failed);
    assert_eq!(result.error_category, Some(CheckinErrorCategory::Auth));
    assert_eq!(fixture.executor.checkin_calls(), 0);
}

struct PoolClosingExecutor {
    inner: Arc<MockCheckinExecutor>,
    storage: Storage,
}

#[async_trait]
impl CheckinExecutor for PoolClosingExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        self.inner.supports(kind)
    }

    async fn check_status(
        &self,
        request: &crate::checkin::CheckinRequest,
        authorization: &crate::account_service::AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        self.inner.check_status(request, authorization).await
    }

    async fn checkin(
        &self,
        request: &crate::checkin::CheckinRequest,
        authorization: &crate::account_service::AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let outcome = self.inner.checkin(request, authorization).await;
        // Simulate the process losing the database right after the upstream
        // accepted the request.
        self.storage.close().await;
        outcome
    }
}

#[tokio::test]
async fn upstream_success_with_failed_local_save_stays_unconfirmed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("polygate.db");
    let storage = Storage::open_file(&path, 4).await.expect("db");
    let clock = TestClock::new(at(2026, 10, 29, 3, 0));
    let inner = Arc::new(MockCheckinExecutor::new());
    let executor = Arc::new(PoolClosingExecutor {
        inner: inner.clone(),
        storage: storage.clone(),
    });
    let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
    let service = CheckinService::new(
        storage.clone(),
        accounts.clone(),
        executor,
        Arc::new(MockQuotaRefreshHook::new()),
    )
    .with_clock(clock.clone());
    let account = accounts
        .create(CreateAccount::new(
            ProviderKind::Codearts,
            Region::Cn,
            "alpha",
            Some(serde_json::json!({"token": "token-alpha"})),
        ))
        .await
        .expect("account");

    let error = service.checkin(&account.id).await.expect_err("save fails");
    assert!(matches!(
        error.kind,
        ErrorKind::Storage | ErrorKind::Internal | ErrorKind::Transport
    ));
    assert_eq!(inner.checkin_calls(), 1);

    // Reopen the same file: the row is still owned by the dead worker.
    let reopened = Storage::open_file(&path, 4).await.expect("reopen");
    let timezone = parse_timezone("Asia/Shanghai").expect("tz");
    let business_date = business_date_at(clock.now(), timezone);
    let record = reopened
        .checkin()
        .get_run(&account.id, business_date)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(record.status, CheckinStatus::Running);

    let recover_executor = Arc::new(MockCheckinExecutor::new());
    let accounts = AccountService::new(reopened.clone(), Arc::new(secret_box()));
    let recovery = CheckinService::new(
        reopened.clone(),
        accounts,
        recover_executor.clone(),
        Arc::new(MockQuotaRefreshHook::new()),
    )
    .with_clock(clock.clone());
    // Lease has expired relative to this clock only after advancing.
    clock.advance(Duration::minutes(11));
    let results = recovery.recover_interrupted().await.expect("recover");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, CheckinStatus::Unknown);
    assert_eq!(recover_executor.checkin_calls(), 0);
    assert_eq!(recover_executor.status_calls(), 1);

    let stored = reopened
        .checkin()
        .get_run(&account.id, business_date)
        .await
        .expect("run")
        .expect("present");
    assert_eq!(stored.status, CheckinStatus::Unknown);
    assert!(!stored.summary.is_empty());
}

// ---------------------------------------------------------------------------
// WorkBuddy check-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_workbuddy_checkin_refreshes_quota_after_success() {
    use crate::checkin::WorkbuddyCheckinExecutor;
    use crate::test_support::{spawn_server, Reply};

    let server = spawn_server(|request| match request.path() {
        "/v2/billing/meter/checkin-activity-status" => Reply::json(
            200,
            serde_json::json!({"code": 0, "data": {"active": true, "today_checked_in": false}})
                .to_string(),
        ),
        "/v2/billing/meter/daily-checkin" => Reply::json(
            200,
            serde_json::json!({"code": 0, "data": {"credit": 100}}).to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let storage = Storage::open_memory().await.expect("memory db");
    let clock = TestClock::new(at(2026, 10, 29, 3, 0));
    let quota = Arc::new(MockQuotaRefreshHook::new());
    let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
    let executor: Arc<dyn CheckinExecutor> = Arc::new(WorkbuddyCheckinExecutor::with_base(
        Region::Cn,
        true,
        reqwest::Client::new(),
        &server.base_url,
    ));
    let service =
        CheckinService::new(storage, accounts.clone(), executor, quota.clone()).with_clock(clock);

    let account = accounts
        .create(CreateAccount::new(
            ProviderKind::Workbuddy,
            Region::Cn,
            "WB",
            Some(serde_json::json!({
                "access_token": "jwt-access",
                "uid": "uid-1",
                "domain": "www.codebuddy.cn"
            })),
        ))
        .await
        .expect("account");

    let result = service.checkin(&account.id).await.expect("checkin");
    assert_eq!(result.status, CheckinStatus::Succeeded, "{result:?}");
    assert_eq!(quota.calls(), 1);
    assert!(result
        .quota_refresh
        .as_ref()
        .is_some_and(|refresh| refresh.attempted && refresh.ok));

    let again = service.checkin(&account.id).await.expect("replay");
    assert_eq!(again.status, CheckinStatus::Succeeded);
    assert!(again.replayed);
    assert_eq!(quota.calls(), 1, "a replay must not refresh quota again");
}

#[tokio::test]
async fn a_disabled_account_is_skipped_by_the_background_loop() {
    let executor = Arc::new(MockCheckinExecutor::support_only([ProviderKind::Workbuddy]));
    let fixture = Fixture::memory_with(Arc::clone(&executor)).await;
    fixture.clock.set(at(2026, 10, 29, 0, 10));
    let account = fixture
        .create(ProviderKind::Workbuddy, Region::Cn, "disabled")
        .await;
    fixture
        .set_schedule(&account, policy("Asia/Shanghai", "00:05"))
        .await;
    fixture
        .accounts
        .set_enabled(&account.id, false)
        .await
        .expect("disable");

    let results = fixture.service.run_due().await.expect("run due");
    assert!(results.is_empty(), "{results:?}");
    assert_eq!(executor.checkin_calls(), 0);
    assert_eq!(executor.status_calls(), 0);
}
