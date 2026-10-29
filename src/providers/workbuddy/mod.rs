//! WorkBuddy provider (CN / International).
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` (M14-A) and the MIT reference
//! implementations it cites. Chat is OpenAI-shaped and upstream is
//! streaming-only, so the non-streaming path aggregates the same stream locally.
//!
//! Compliance boundary: the default (`Identity::Honest`) sends only the headers
//! the credential and protocol require (Bearer, account ids, domain) plus the
//! gateway's own User-Agent. With `Identity::Impersonate` the desktop client's
//! header family is replayed from [`crate::providers::profile`]; device tokens
//! (Turing Shield) and prompt rewriting remain absent.
//!
//! There is no upstream session id and no cancellation endpoint, so
//! `native_session`, `agent_task` and `cancellation` stay `false`.

pub mod auth;
pub mod convert;
pub mod credential;
pub mod error;
pub mod events;
pub mod http;
pub mod models;
pub mod provider;
pub mod quota;
pub mod region;

#[cfg(test)]
mod tests;
