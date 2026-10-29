//! PolyGate — multi-provider coding agent gateway.
//!
//! Stage 02 (Rust Core) baseline. Only modules that already contain code are
//! declared here; `scheduler` and `metrics` are empty placeholders reserved for
//! later milestones.

#![forbid(unsafe_code)]

pub mod account_service;
pub mod accounts;
pub mod admin;
pub mod api;
pub mod checkin;
pub mod cli;
pub mod config;
pub mod error;
pub mod model_service;
pub mod models;
pub mod protocol;
pub mod providers;
pub mod quota;
pub mod runtime;
pub mod scheduler;
pub mod security;
pub mod session_service;
pub mod sessions;
pub mod storage;

#[cfg(test)]
pub(crate) mod test_support;

/// Crate version, also used by the binary entry point.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
