//! Qoder protocol foundation (M13-D2-A) and the inference adapter (M13-D2-C).
//!
//! D2-A defines realm endpoints, COSY request signing, error classification,
//! chat/tool conversion and SSE parsing. D2-B adds device-code authorization and
//! credential persistence. D2-C adds the OpenAPI quota probe, the check-in
//! campaign client and the COSY-signed model list and chat endpoints.
//!
//! Evidence: `docs/QODER_PROVIDER_RESEARCH.md` and Coding2API commit
//! `df9936240708f99f5a1cd92e52330b4b2016c840` (MIT; protocol facts only, no code
//! copied). Every public item states what is source-evidenced and what remains
//! unverified.
//!
//! Client identity: `Identity::Impersonate` adds the desktop client's `cosy-*`
//! device family (derived per uid) to signed and OpenAPI requests; the default
//! sends the signed headers only.
//!
//! Region safety: callers must pass an explicit [`crate::config::Region`].
//! [`region::realm`] has no fallback; CN and Intl credentials, hosts and signed
//! requests never cross.

pub mod auth;
pub mod convert;
pub mod cosy;
pub mod credential;
pub mod error;
pub mod events;
pub mod gateway;
pub mod http;
pub mod provider;
pub mod quota;
pub mod region;

#[cfg(test)]
mod tests;
