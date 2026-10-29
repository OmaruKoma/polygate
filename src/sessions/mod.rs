//! Downstream sessions and their upstream binding.
//!
//! A gateway session is identified by the client supplied session id (header or
//! request field). It is bound to exactly one account and (optionally) one
//! upstream native session id. Binding is persisted so a restart keeps valid
//! sessions.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Expired,
    Invalid,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Active => "active",
            SessionState::Expired => "expired",
            SessionState::Invalid => "invalid",
        }
    }
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "active" => SessionState::Active,
            "expired" => SessionState::Expired,
            _ => SessionState::Invalid,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: String,
    /// Client supplied id; the binding lookup key together with provider kind
    /// and region.
    pub client_session_id: String,
    pub provider: String,
    pub kind: crate::config::ProviderKind,
    pub region: String,
    pub account_id: String,
    pub upstream_session_id: Option<String>,
    /// State of the persisted binding, independent from the upstream session.
    pub state: SessionState,
    /// Compare-and-swap token for binding changes (see migration 0004).
    pub binding_revision: i64,
    /// Set when the binding became invalid; kept until an explicit recovery.
    pub invalid_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub request_count: u64,
    pub metadata: serde_json::Value,
}

impl Session {
    pub fn is_active(&self) -> bool {
        self.state == SessionState::Active && self.expires_at > Utc::now()
    }
    pub fn expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }
    pub fn refresh_expiry(&mut self, ttl_secs: u64) {
        self.last_used_at = Utc::now();
        self.expires_at = Utc::now() + Duration::seconds(ttl_secs as i64);
    }
}

/// Extract a stable client session id from request headers, if present.
///
/// Supported conventions (checked in order):
/// * `x-session-id` / `x-polygate-session-id`
/// * OpenAI `x-stainless-*` clients do not send one; callers then derive an id
///   from `metadata.session_id` or the conversation prefix.
/// * Anthropic `x-session-id` (Claude Code sends it in recent versions).
pub fn session_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    for name in ["x-session-id", "x-polygate-session-id", "session_id"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let v = v.trim();
            if !v.is_empty() && v.len() <= 200 {
                return Some(v.to_string());
            }
        }
    }
    None
}
