//! Session binding policy (M13).
//!
//! ```text
//! session_service -> sessions (domain), storage, account_service, error
//! runtime         -> session_service
//! future scheduler -> session_service (bind/rebind) + sessions (read-only)
//! ```
//!
//! `sessions` stays a domain module; the repository keeps the SQL and this layer
//! owns stickiness, compare-and-swap updates and concurrency permits.

mod limits;
mod router;
mod service;

#[cfg(test)]
mod tests;

pub use limits::{AccountLease, AccountLimiter, LimitPolicy, SessionGate, SessionGateGuard};
pub use router::{RouteLease, SessionRouter};
pub use service::{
    classify_session_failure, BindOutcome, SessionBindingService, SessionFailure, SessionKey,
    SessionLease, SessionRecovery,
};
