//! Huawei OfficeAce (AgentArts office assistant) provider adapter.
//!
//! Protocol evidence (read-only): Coding2API `src/provider/officeace/*` (MIT)
//! and its `TECHNICAL.md` §5.6, which records measurements against real
//! accounts. Only protocol facts were absorbed; the implementation is
//! independent Rust.
//!
//! What makes this adapter different from the other providers:
//!
//! 1. **Chat is standard OpenAI** (`{gateway}/v2/chat/completions`, `Basic`
//!    auth). No private envelope, so [`convert`] and [`events`] are a plain
//!    projection; `stream` is always requested and non-streaming calls
//!    aggregate locally.
//! 2. **Two credential families** with different lifetimes: the `Basic` app key
//!    never expires, the AK/SK for quota and check-in lives about two hours.
//!    Mixing them answers `APIG.0303`.
//! 3. **Two signature tracks**: `/v1/subscription*` on the model gateway wants
//!    `V11-HMAC-SHA256`; `/v1/claw/*` on the control plane wants
//!    `SDK-HMAC-SHA256`. See [`signer`].
//! 4. **Points are the main ledger** (`officeace_points`): `remaining`/`total`
//!    are points, not tokens, and the daily bonus is claimed explicitly (see
//!    `crate::checkin::officeace`).
//!
//! Verification state is `experimental` in code: nothing here has been run
//! against a real OfficeAce account by this project yet.

mod convert;
pub mod credential;
pub mod events;
pub mod http;
pub mod login;
pub mod models;
pub mod provider;
pub mod region;
pub mod signer;
pub mod subscription;

#[cfg(test)]
mod tests;

pub use credential::OfficeAceCredential;
pub use http::OfficeAceHttp;
pub use provider::OfficeAceProvider;
