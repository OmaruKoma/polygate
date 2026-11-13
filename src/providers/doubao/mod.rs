//! Doubao (豆包 / DoubaoWork) provider.
//!
//! Evidence: `docs/DOUBAO_MIGRATION.md`, derived from the Apache-2.0 reference
//! gateway `2951461586/Doubao2API` and a live probe against the real upstream on
//! 2026-11-13. Only protocol facts were reused; no reference code was copied.
//!
//! What this adapter deliberately does **not** do:
//! * it never logs in on the operator's behalf (no password, no QR flow);
//! * it never generates a device identity (`device_id` / `web_id` / `fp`) —
//!   measured: the upstream treats an unknown device as a risk signal and
//!   answers `710022004 rate limited`, so the values belonging to the imported
//!   login are the only ones used;
//! * it does not impersonate the desktop client: `Origin`, `Referer` and a
//!   browser User-Agent are omitted, which the upstream accepts;
//! * it does not call image/file/skill generation surfaces.
//!
//! Current scope: text chat (fast / think / expert tiers) with streaming and
//! non-streaming shapes, plus the reasoning chain. Image input is reported as
//! unsupported because the upstream only accepts its own resource-centre `uri`.

mod credential;
mod events;
mod http;
mod payload;
pub mod provider;
mod quota;
pub mod region;

#[cfg(test)]
mod tests;

pub use credential::DoubaoCredential;
pub use http::DoubaoHttp;
pub use provider::DoubaoProvider;
