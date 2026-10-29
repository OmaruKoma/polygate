//! TRAE provider (CN / International).
//!
//! Evidence: `docs/TRAE_MIGRATION.md` and the sources it cites
//! (`Coding2API/src/provider/trae/*`, `dsh-wtq-pool/src/trae-*.ts`).
//!
//! M14-D implements the protocol core: credential parsing, region isolation,
//! model discovery, streaming and non-streaming chat, tool calling, usage and
//! error mapping. Token refresh, quota and check-in are M14-E.
//!
//! Compliance boundary: by default the adapter authenticates with the token
//! headers TRAE needs and nothing else. `Identity::Impersonate` adds the IDE's
//! static header family from [`crate::providers::profile`]. Either way device
//! identifiers are never generated — only the operator's imported ids are sent. There is no upstream session id and no cancellation endpoint, so
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
