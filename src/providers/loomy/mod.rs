//! Loomy (iFlytek desktop assistant) provider.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md`, derived from the official Electron
//! client 0.9.38 and the MIT reference `Patrick130306/loomy2api`. Only protocol
//! facts were reused; no reference code was copied.
//!
//! What this adapter deliberately does **not** do:
//! * it never logs in with a phone number and password, and never stores one;
//! * it never generates or rotates a device identity (`devid`, promotions
//!   `campus_device_id`) — the operator imports an existing session;
//! * it never redeems onboarding/invitation rewards.
//!
//! The imported session is a 14-day bearer with no refresh token, so the admin
//! UI reports its age and asks for a new one when the upstream rejects it.

mod convert;
mod credential;
mod events;
mod http;
mod models;
mod quota;
pub mod region;

pub mod provider;

#[cfg(test)]
mod tests;

pub use credential::LoomyCredential;
pub use http::LoomyHttp;
pub use provider::LoomyProvider;
