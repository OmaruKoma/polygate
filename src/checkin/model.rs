//! Check-in domain model.
//!
//! Everything here is protocol free: no HTTP, no provider adapter and no
//! account secret. The state machine is deliberately small and every state is
//! persisted; `Unknown` means "the request may have reached the upstream but the
//! final result cannot be confirmed" and is never folded into `Failed` and never
//! retried blindly.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};

/// Maximum number of accounts accepted by one manual batch call.
pub const MAX_BATCH_ACCOUNTS: usize = 200;
/// In-flight concurrency for manual batches and the automatic loop.
pub const BATCH_CONCURRENCY: usize = 4;
/// How long a `running` row may stay owned before it is treated as interrupted.
pub const DEFAULT_LEASE_SECS: i64 = 600;
/// Default interval between scheduler ticks.
pub const DEFAULT_TICK_SECS: u64 = 60;

/// Lifecycle of one (account, business date) check-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckinStatus {
    /// Row created, no worker owns it yet.
    Pending,
    /// A worker owns the row and is talking to the upstream.
    Running,
    /// Upstream confirmed the check-in in this run.
    Succeeded,
    /// Upstream reported that today's check-in was already done.
    AlreadyCheckedIn,
    /// Upstream (or the local side) definitively rejected the check-in.
    Failed,
    /// The request may have reached the upstream; the outcome cannot be
    /// confirmed. Never auto-retried.
    Unknown,
    /// The provider has no check-in capability.
    Unsupported,
}

impl CheckinStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckinStatus::Pending => "pending",
            CheckinStatus::Running => "running",
            CheckinStatus::Succeeded => "succeeded",
            CheckinStatus::AlreadyCheckedIn => "already_checked_in",
            CheckinStatus::Failed => "failed",
            CheckinStatus::Unknown => "unknown",
            CheckinStatus::Unsupported => "unsupported",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "succeeded" => CheckinStatus::Succeeded,
            "already_checked_in" => CheckinStatus::AlreadyCheckedIn,
            "failed" => CheckinStatus::Failed,
            "unknown" => CheckinStatus::Unknown,
            "unsupported" => CheckinStatus::Unsupported,
            "running" => CheckinStatus::Running,
            _ => CheckinStatus::Pending,
        }
    }

    /// A successful or already-done check-in: quota should be refreshed and the
    /// credential is proven usable.
    pub const fn is_success(self) -> bool {
        matches!(
            self,
            CheckinStatus::Succeeded | CheckinStatus::AlreadyCheckedIn
        )
    }

    /// No worker currently owns the row. `Failed` is settled but may still be
    /// retried; `Unknown` is settled *and* must not be retried without an
    /// upstream status query.
    pub const fn is_settled(self) -> bool {
        !matches!(self, CheckinStatus::Pending | CheckinStatus::Running)
    }

    /// Whether an automatic loop may pick a failed run up again.
    pub fn allows_auto_retry(self) -> bool {
        self == CheckinStatus::Failed
    }
}

/// Coarse category stored in `last_error_category`. It is intentionally
/// independent from [`ErrorKind`]: the database should keep a stable, small
/// vocabulary even if the error taxonomy grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckinErrorCategory {
    Timeout,
    Transport,
    Upstream,
    Auth,
    RateLimit,
    Quota,
    Unsupported,
    Storage,
    Other,
}

impl CheckinErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckinErrorCategory::Timeout => "timeout",
            CheckinErrorCategory::Transport => "transport",
            CheckinErrorCategory::Upstream => "upstream",
            CheckinErrorCategory::Auth => "auth",
            CheckinErrorCategory::RateLimit => "rate_limit",
            CheckinErrorCategory::Quota => "quota",
            CheckinErrorCategory::Unsupported => "unsupported",
            CheckinErrorCategory::Storage => "storage",
            CheckinErrorCategory::Other => "other",
        }
    }

    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "timeout" => Some(CheckinErrorCategory::Timeout),
            "transport" => Some(CheckinErrorCategory::Transport),
            "upstream" => Some(CheckinErrorCategory::Upstream),
            "auth" => Some(CheckinErrorCategory::Auth),
            "rate_limit" => Some(CheckinErrorCategory::RateLimit),
            "quota" => Some(CheckinErrorCategory::Quota),
            "unsupported" => Some(CheckinErrorCategory::Unsupported),
            "storage" => Some(CheckinErrorCategory::Storage),
            "other" => Some(CheckinErrorCategory::Other),
            _ => None,
        }
    }

    pub fn from_error_kind(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::Timeout => CheckinErrorCategory::Timeout,
            ErrorKind::Transport | ErrorKind::UpstreamServer => CheckinErrorCategory::Transport,
            ErrorKind::AuthInvalid
            | ErrorKind::AuthExpired
            | ErrorKind::AccountDisabled
            | ErrorKind::ChannelRestricted => CheckinErrorCategory::Auth,
            ErrorKind::RateLimited => CheckinErrorCategory::RateLimit,
            ErrorKind::QuotaExhausted => CheckinErrorCategory::Quota,
            ErrorKind::Unsupported => CheckinErrorCategory::Unsupported,
            ErrorKind::Storage => CheckinErrorCategory::Storage,
            _ => CheckinErrorCategory::Other,
        }
    }

    /// Only a definitive, side-effect-free rejection is worth an automatic
    /// retry. Timeout/transport/upstream failures land in `Unknown`, which the
    /// automatic loop never retries.
    pub fn is_auto_retryable(self) -> bool {
        matches!(self, CheckinErrorCategory::RateLimit)
    }
}

/// One check-in attempt description. No secret is part of this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckinRequest {
    pub account_id: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub business_date: NaiveDate,
    pub timezone: String,
    /// 1-based attempt number for this business date.
    pub attempt: u32,
}

/// What an executor reports after a check-in call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckinOutcome {
    Succeeded {
        summary: String,
    },
    AlreadyCheckedIn {
        summary: String,
    },
    Failed {
        category: CheckinErrorCategory,
        summary: String,
    },
    /// The call was sent but the result is not confirmable (timeout after the
    /// request was written, connection reset, ...).
    Unknown {
        summary: String,
    },
    Unsupported,
}

impl CheckinOutcome {
    pub fn status(&self) -> CheckinStatus {
        match self {
            CheckinOutcome::Succeeded { .. } => CheckinStatus::Succeeded,
            CheckinOutcome::AlreadyCheckedIn { .. } => CheckinStatus::AlreadyCheckedIn,
            CheckinOutcome::Failed { .. } => CheckinStatus::Failed,
            CheckinOutcome::Unknown { .. } => CheckinStatus::Unknown,
            CheckinOutcome::Unsupported => CheckinStatus::Unsupported,
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            CheckinOutcome::Succeeded { summary }
            | CheckinOutcome::AlreadyCheckedIn { summary }
            | CheckinOutcome::Failed { summary, .. }
            | CheckinOutcome::Unknown { summary } => summary,
            CheckinOutcome::Unsupported => "provider does not support check-in",
        }
    }

    pub fn category(&self) -> Option<CheckinErrorCategory> {
        match self {
            CheckinOutcome::Failed { category, .. } => Some(*category),
            _ => None,
        }
    }
}

/// Per-account result of a manual or automatic run.
#[derive(Debug, Clone, Serialize)]
pub struct CheckinResult {
    pub account_id: String,
    pub business_date: NaiveDate,
    pub status: CheckinStatus,
    /// `true` when an existing record was returned instead of executing again
    /// (same-day duplicate, running lease owned by another worker, ...).
    pub replayed: bool,
    pub attempt_count: u32,
    pub error_category: Option<CheckinErrorCategory>,
    pub summary: String,
    /// Present only when this call produced (or confirmed) a successful
    /// check-in and the quota hook was invoked.
    pub quota_refresh: Option<QuotaRefreshOutcome>,
}

/// Outcome of the post-check-in quota refresh. A failure here never rolls the
/// check-in back to `Failed`.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaRefreshOutcome {
    pub attempted: bool,
    pub ok: bool,
    pub message: String,
}

/// Persisted run row.
#[derive(Debug, Clone)]
pub struct CheckinRecord {
    pub id: String,
    pub account_id: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub business_date: NaiveDate,
    pub timezone: String,
    pub status: CheckinStatus,
    pub attempt_count: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error_category: Option<CheckinErrorCategory>,
    pub summary: String,
    pub lease_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CheckinRecord {
    pub fn to_result(&self, replayed: bool) -> CheckinResult {
        CheckinResult {
            account_id: self.account_id.clone(),
            business_date: self.business_date,
            status: self.status,
            replayed,
            attempt_count: self.attempt_count,
            error_category: self.error_category,
            summary: self.summary.clone(),
            quota_refresh: None,
        }
    }
}

/// How a claim may treat an existing `failed` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckinRetryMode {
    /// Operator explicitly asked again; attempt count still increments.
    Manual,
    /// Automatic loop: only retry a definitively failed, retryable category
    /// while the attempt budget lasts.
    Automatic { max_attempts: u32 },
}

impl CheckinRetryMode {
    pub fn is_manual(&self) -> bool {
        matches!(self, CheckinRetryMode::Manual)
    }

    pub fn allows_failed(&self, record: &CheckinRecord) -> bool {
        match self {
            CheckinRetryMode::Manual => true,
            CheckinRetryMode::Automatic { max_attempts } => {
                record.attempt_count < *max_attempts
                    && record
                        .error_category
                        .is_some_and(CheckinErrorCategory::is_auto_retryable)
            }
        }
    }
}

/// Check-in schedule for one account, in the provider's time zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckinPolicy {
    pub enabled: bool,
    /// IANA name (for example `Asia/Shanghai`). Never the host's local zone.
    pub timezone: String,
    /// `HH:MM`, 24h, interpreted in `timezone`.
    pub local_time: String,
    /// Maximum random-ish delay added after `local_time`. Stable per
    /// (account, business date) so a restart does not reschedule the day.
    pub jitter_secs: u64,
    /// How long after the occurrence a missed run may still be executed.
    pub catch_up_secs: u64,
    /// Attempt budget for automatic retries of a definitively failed run.
    pub max_attempts: u32,
}

impl Default for CheckinPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            timezone: "Asia/Shanghai".into(),
            local_time: "00:05".into(),
            jitter_secs: 0,
            catch_up_secs: 3600,
            max_attempts: 2,
        }
    }
}

impl CheckinPolicy {
    pub fn validate(&self) -> Result<(), GatewayError> {
        parse_timezone(&self.timezone)?;
        parse_local_time(&self.local_time)?;
        if self.jitter_secs > 86_400 {
            return Err(GatewayError::bad_request("check-in jitter must be <= 24h"));
        }
        if self.catch_up_secs > 86_400 {
            return Err(GatewayError::bad_request(
                "check-in catch-up window must be <= 24h",
            ));
        }
        if !(1..=10).contains(&self.max_attempts) {
            return Err(GatewayError::bad_request(
                "check-in max_attempts must be within 1..=10",
            ));
        }
        Ok(())
    }
}

/// Schedule attached to one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckinSchedule {
    pub account_id: String,
    pub policy: CheckinPolicy,
}

/// Wall clock abstraction. Tests inject a fixed clock; production uses
/// [`SystemClock`]. Business dates are always derived through this interface.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Provider default time zone used when an account has no schedule yet.
///
/// The M10 research only established CN-side check-in endpoints; the Intl
/// variants either do not exist or are not verified. This mapping is therefore a
/// placeholder and lives in one place so M13 providers can replace it with the
/// declared provider time zone instead of sprinkling offsets around.
pub fn default_timezone(region: Region) -> &'static str {
    match region {
        Region::Cn => "Asia/Shanghai",
        Region::Intl => "UTC",
    }
}

pub fn parse_timezone(name: &str) -> Result<Tz, GatewayError> {
    name.trim()
        .parse::<Tz>()
        .map_err(|_| GatewayError::bad_request(format!("unknown time zone `{name}`")))
}

pub fn parse_local_time(value: &str) -> Result<NaiveTime, GatewayError> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .map_err(|_| GatewayError::bad_request("local_time must be HH:MM (24h)"))
}

pub fn business_date_at(instant: DateTime<Utc>, timezone: Tz) -> NaiveDate {
    instant.with_timezone(&timezone).date_naive()
}

/// UTC instant of `date` + `time` in `timezone`, resolving DST edge cases:
/// ambiguous local times take the earlier instant, skipped local times move
/// forward until a valid instant exists.
pub fn local_occurrence(date: NaiveDate, time: NaiveTime, timezone: Tz) -> DateTime<Utc> {
    let naive = date.and_time(time);
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(value) => value.with_timezone(&Utc),
        LocalResult::Ambiguous(earlier, later) => earlier.min(later).with_timezone(&Utc),
        LocalResult::None => {
            for minutes in 1..=180 {
                let shifted = naive + Duration::minutes(minutes);
                if let LocalResult::Single(value) = timezone.from_local_datetime(&shifted) {
                    return value.with_timezone(&Utc);
                }
            }
            naive.and_utc()
        }
    }
}

/// Stable per-(account, date) jitter. A restart on the same day must not move the
/// occurrence, otherwise the catch-up window / dedupe would behave differently
/// after every process restart.
pub fn jitter_offset(account_id: &str, date: NaiveDate, max_secs: u64) -> u64 {
    if max_secs == 0 {
        return 0;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in account_id
        .bytes()
        .chain(date.format("%Y-%m-%d").to_string().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash % (max_secs + 1)
}

/// Business date that is currently due for this schedule, if any.
///
/// Both the local date in the provider time zone and the previous local date are
/// considered so a catch-up window may span midnight. Returns the *business date*
/// of the occurrence, not the current date.
pub fn due_business_date(
    account_id: &str,
    policy: &CheckinPolicy,
    timezone: Tz,
    now: DateTime<Utc>,
) -> Option<NaiveDate> {
    if !policy.enabled {
        return None;
    }
    let time = parse_local_time(&policy.local_time).ok()?;
    let today = business_date_at(now, timezone);
    let catch_up = Duration::seconds(policy.catch_up_secs as i64);
    for date in [today, today.pred_opt()?] {
        let occurrence = local_occurrence(date, time, timezone)
            + Duration::seconds(jitter_offset(account_id, date, policy.jitter_secs) as i64);
        if occurrence <= now && now < occurrence + catch_up {
            return Some(date);
        }
    }
    None
}

/// Explain a failed upstream call: whether the side effect may have happened
/// and which category to persist.
///
/// Timeout, transport failure, upstream 5xx and local persistence failure all
/// land in `Unknown` because the request may already have been applied. Only a
/// definitive rejection (auth, rate limit, quota, unsupported) becomes `Failed`.
pub fn classify_error(error: &GatewayError) -> (CheckinStatus, CheckinErrorCategory) {
    let category = CheckinErrorCategory::from_error_kind(error.kind);
    let status = match error.kind {
        ErrorKind::AuthInvalid
        | ErrorKind::AuthExpired
        | ErrorKind::AccountDisabled
        | ErrorKind::ChannelRestricted
        | ErrorKind::RateLimited
        | ErrorKind::QuotaExhausted
        | ErrorKind::BadRequest => CheckinStatus::Failed,
        ErrorKind::Unsupported => CheckinStatus::Unsupported,
        _ => CheckinStatus::Unknown,
    };
    (status, category)
}

/// The stable category used when a probe reports a definitive upstream failure.
pub fn probe_failure_category() -> CheckinErrorCategory {
    CheckinErrorCategory::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    #[test]
    fn status_roundtrips() {
        for status in [
            CheckinStatus::Pending,
            CheckinStatus::Running,
            CheckinStatus::Succeeded,
            CheckinStatus::AlreadyCheckedIn,
            CheckinStatus::Failed,
            CheckinStatus::Unknown,
            CheckinStatus::Unsupported,
        ] {
            assert_eq!(CheckinStatus::from_db_str(status.as_str()), status);
        }
    }

    #[test]
    fn unknown_is_terminal_but_not_success() {
        assert!(CheckinStatus::Unknown.is_settled());
        assert!(!CheckinStatus::Unknown.is_success());
        assert!(!CheckinStatus::Unknown.allows_auto_retry());
        assert!(CheckinStatus::Failed.is_settled());
        assert!(!CheckinStatus::Failed.is_success());
        assert!(CheckinStatus::Failed.allows_auto_retry());
    }

    #[test]
    fn business_date_uses_timezone_not_host_clock() {
        let instant = Utc
            .with_ymd_and_hms(2026, 10, 29, 16, 30, 0)
            .single()
            .expect("instant");
        let cn = parse_timezone("Asia/Shanghai").expect("tz");
        let utc = parse_timezone("UTC").expect("tz");
        assert_eq!(business_date_at(instant, cn), date(2026, 10, 30));
        assert_eq!(business_date_at(instant, utc), date(2026, 10, 29));
    }

    #[test]
    fn jitter_is_stable_and_bounded() {
        let first = jitter_offset("account-a", date(2026, 10, 29), 3600);
        let again = jitter_offset("account-a", date(2026, 10, 29), 3600);
        let other = jitter_offset("account-b", date(2026, 10, 29), 3600);
        assert_eq!(first, again);
        assert!(first <= 3600);
        assert!(other <= 3600);
        assert_eq!(jitter_offset("account-a", date(2026, 10, 29), 0), 0);
    }

    #[test]
    fn due_window_covers_catch_up_and_previous_date() {
        let tz = parse_timezone("UTC").expect("tz");
        let policy = CheckinPolicy {
            enabled: true,
            timezone: "UTC".into(),
            local_time: "00:00".into(),
            jitter_secs: 0,
            catch_up_secs: 3600,
            max_attempts: 2,
        };
        let at_ten_past = Utc.with_ymd_and_hms(2026, 10, 29, 0, 10, 0).unwrap();
        assert_eq!(
            due_business_date("a", &policy, tz, at_ten_past),
            Some(date(2026, 10, 29))
        );
        let two_hours_later = Utc.with_ymd_and_hms(2026, 10, 29, 2, 0, 0).unwrap();
        assert_eq!(due_business_date("a", &policy, tz, two_hours_later), None);
        // 00:10 on the 30th: the 30th is not due yet (its occurrence is 00:00 but
        // catch-up of the 29th has expired), the 29th occurrence is 24h old.
        let next_day = Utc.with_ymd_and_hms(2026, 10, 30, 0, 10, 0).unwrap();
        assert_eq!(
            due_business_date("a", &policy, tz, next_day),
            Some(date(2026, 10, 30))
        );
    }

    #[test]
    fn catch_up_crosses_midnight() {
        let tz = parse_timezone("UTC").expect("tz");
        let policy = CheckinPolicy {
            enabled: true,
            timezone: "UTC".into(),
            local_time: "23:40".into(),
            jitter_secs: 0,
            catch_up_secs: 3600,
            max_attempts: 2,
        };
        let after_midnight = Utc.with_ymd_and_hms(2026, 10, 30, 0, 10, 0).unwrap();
        assert_eq!(
            due_business_date("a", &policy, tz, after_midnight),
            Some(date(2026, 10, 29))
        );
    }

    #[test]
    fn disabled_policy_is_never_due() {
        let tz = parse_timezone("UTC").expect("tz");
        let policy = CheckinPolicy {
            enabled: false,
            ..CheckinPolicy::default()
        };
        let now = Utc::now();
        assert_eq!(due_business_date("a", &policy, tz, now), None);
    }

    #[test]
    fn policy_validation_rejects_bad_values() {
        let bad_tz = CheckinPolicy {
            timezone: "Mars/Olympus".into(),
            ..CheckinPolicy::default()
        };
        assert!(bad_tz.validate().is_err());
        let bad_time = CheckinPolicy {
            local_time: "25:99".into(),
            ..CheckinPolicy::default()
        };
        assert!(bad_time.validate().is_err());
        let bad_attempts = CheckinPolicy {
            max_attempts: 0,
            ..CheckinPolicy::default()
        };
        assert!(bad_attempts.validate().is_err());
        assert!(CheckinPolicy::default().validate().is_ok());
    }

    #[test]
    fn classification_never_treats_timeout_as_failed() {
        let error = GatewayError::timeout("no answer");
        let (status, category) = classify_error(&error);
        assert_eq!(status, CheckinStatus::Unknown);
        assert_eq!(category, CheckinErrorCategory::Timeout);

        let error = GatewayError::auth_invalid("bad token");
        let (status, category) = classify_error(&error);
        assert_eq!(status, CheckinStatus::Failed);
        assert_eq!(category, CheckinErrorCategory::Auth);

        let error = GatewayError::upstream_server("500");
        let (status, _) = classify_error(&error);
        assert_eq!(status, CheckinStatus::Unknown);
    }
}
