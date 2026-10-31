//! Persistent check-in engine (M12).
//!
//! Dependency direction:
//!
//! ```text
//! checkin (service) -> account_service, storage, error
//! checkin (model)   -> config, error
//! checkin (executor)-> account_service (authorization context only)
//! storage           -> checkin (record/schedule rows)
//! runtime           -> checkin
//! ```
//!
//! The provider registry is deliberately **not** involved: the runtime wires each
//! real adapter as its own [`CheckinExecutor`], and an [`AuthRefreshHook`]
//! (also runtime-owned) freshens an expiring credential before the call. This
//! module never bypasses a captcha and never queries real quota.

mod auth;
mod codearts;
mod executor;
mod model;
mod officeace;
mod qoder;
mod quota;
mod scheduler;
mod service;
mod trae;
mod workbuddy;

#[cfg(test)]
mod tests;

pub use auth::{AuthRefreshHook, AuthRefreshRequest, MockAuthRefreshHook, NoopAuthRefresh};
pub use codearts::CodeArtsCheckinExecutor;
pub use executor::{
    CheckinExecutor, CheckinProbe, MockCheckinExecutor, MockCheckinStep, RoutingCheckinExecutor,
};
pub use model::{
    business_date_at, classify_error, default_timezone, due_business_date, jitter_offset,
    local_occurrence, parse_local_time, parse_timezone, probe_failure_category,
    CheckinErrorCategory, CheckinOutcome, CheckinPolicy, CheckinRecord, CheckinRequest,
    CheckinResult, CheckinRetryMode, CheckinSchedule, CheckinStatus, Clock, QuotaRefreshOutcome,
    SystemClock, BATCH_CONCURRENCY, DEFAULT_LEASE_SECS, DEFAULT_TICK_SECS, MAX_BATCH_ACCOUNTS,
};
pub use officeace::OfficeAceCheckinExecutor;
pub use qoder::QoderCheckinExecutor;
pub use quota::{MockQuotaRefreshHook, QuotaRefreshHook, QuotaRefreshRequest};
pub use scheduler::{CheckinScheduler, Sleeper, TokioSleeper};
pub use service::CheckinService;
pub use trae::TraeCheckinExecutor;
pub use workbuddy::WorkbuddyCheckinExecutor;
