//! TeleAgent (China Telecom 星辰超级智能体) provider.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md`, reverse engineered from the
//! desktop client 3.1.0 on 2026-11-18 (Go engine `super-agent-code`, signed
//! `X-SA-*` request set) and exercised against the live upstream.
//!
//! What this adapter deliberately does **not** do:
//! * it never stores a phone number or password and never logs in;
//! * it never generates a device identity: `deviceId` / `installId` are
//!   imported from the client (`device-meta.json`);
//! * it never calls the operator's local engine on `127.0.0.1:4397`, whose
//!   signing key lives only in the Electron process memory.
//!
//! The imported token is a 7-day JWT with no refresh token, so the operator
//! re-imports it when the upstream answers `40108`.

mod convert;
mod credential;
mod events;
mod http;
mod models;
pub mod provider;
mod quota;
pub mod region;
mod signer;

#[cfg(test)]
mod tests;

pub use credential::TeleagentCredential;
pub use http::TeleagentHttp;
pub use provider::TeleagentProvider;
