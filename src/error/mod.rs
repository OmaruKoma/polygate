//! Unified error type and upstream error classification.
//!
//! Every failure inside the gateway is a [`GatewayError`] carrying a stable
//! [`ErrorKind`]. The kind decides the HTTP status, the machine readable code,
//! the coarse [`ErrorCategory`] used by scheduling/metrics, and the OpenAI or
//! Anthropic shaped body returned to a downstream client.
//!
//! Invariants:
//! * [`GatewayError::detail`] is diagnostic only and is never serialized.
//! * Messages and details pass through [`redact_secrets`] before they can reach
//!   a response body or a log line.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Classification of a failure, independent of any wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The request itself is malformed (bad JSON, bad parameters).
    BadRequest,
    /// The credential was rejected upstream (or by the gateway key check).
    AuthInvalid,
    /// The credential expired and must be refreshed by the operator.
    AuthExpired,
    /// The account/credential has been disabled or banned upstream.
    AccountDisabled,
    /// The upstream enforces a channel/plan restriction for this account.
    ChannelRestricted,
    /// The account's quota/credits are exhausted.
    QuotaExhausted,
    /// Upstream or gateway rate limit.
    RateLimited,
    /// No healthy account is currently available.
    NoCapacity,
    /// The requested model does not exist or is not enabled for this account.
    ModelUnavailable,
    /// The bound session no longer points at a usable account.
    SessionBindingInvalid,
    /// The provider does not implement this capability.
    Unsupported,
    /// Upstream returned an unexpected 5xx / malformed payload.
    UpstreamServer,
    /// Network failure (DNS, TLS, connection reset).
    Transport,
    /// Upstream did not answer in time.
    Timeout,
    /// The request was cancelled by the client or the task manager.
    Cancelled,
    /// Invalid local configuration (file, address, master key, ...).
    Config,
    /// Local database/storage failure.
    Storage,
    /// Any other local failure.
    Internal,
}

impl ErrorKind {
    /// Every kind, used by table driven tests and admin listings.
    pub const ALL: &'static [ErrorKind] = &[
        ErrorKind::BadRequest,
        ErrorKind::AuthInvalid,
        ErrorKind::AuthExpired,
        ErrorKind::AccountDisabled,
        ErrorKind::ChannelRestricted,
        ErrorKind::QuotaExhausted,
        ErrorKind::RateLimited,
        ErrorKind::NoCapacity,
        ErrorKind::ModelUnavailable,
        ErrorKind::SessionBindingInvalid,
        ErrorKind::Unsupported,
        ErrorKind::UpstreamServer,
        ErrorKind::Transport,
        ErrorKind::Timeout,
        ErrorKind::Cancelled,
        ErrorKind::Config,
        ErrorKind::Storage,
        ErrorKind::Internal,
    ];

    /// Coarse failure domain, stable across protocol adapters.
    pub fn category(self) -> ErrorCategory {
        match self {
            ErrorKind::BadRequest => ErrorCategory::Request,
            ErrorKind::AuthInvalid
            | ErrorKind::AuthExpired
            | ErrorKind::AccountDisabled
            | ErrorKind::ChannelRestricted => ErrorCategory::Auth,
            ErrorKind::QuotaExhausted | ErrorKind::RateLimited => ErrorCategory::Quota,
            ErrorKind::NoCapacity
            | ErrorKind::ModelUnavailable
            | ErrorKind::SessionBindingInvalid => ErrorCategory::Routing,
            ErrorKind::UpstreamServer | ErrorKind::Transport => ErrorCategory::Provider,
            ErrorKind::Timeout => ErrorCategory::Timeout,
            ErrorKind::Cancelled => ErrorCategory::Cancelled,
            ErrorKind::Unsupported => ErrorCategory::Unsupported,
            ErrorKind::Config => ErrorCategory::Config,
            ErrorKind::Storage => ErrorCategory::Database,
            ErrorKind::Internal => ErrorCategory::Internal,
        }
    }

    /// HTTP status used by the OpenAI/Anthropic compatible surface.
    pub fn http_status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            ErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            ErrorKind::AuthInvalid | ErrorKind::AuthExpired => StatusCode::UNAUTHORIZED,
            ErrorKind::AccountDisabled | ErrorKind::ChannelRestricted => StatusCode::FORBIDDEN,
            ErrorKind::QuotaExhausted => StatusCode::PAYMENT_REQUIRED,
            ErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorKind::NoCapacity => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::ModelUnavailable => StatusCode::NOT_FOUND,
            ErrorKind::SessionBindingInvalid => StatusCode::CONFLICT,
            ErrorKind::Unsupported => StatusCode::NOT_IMPLEMENTED,
            ErrorKind::UpstreamServer | ErrorKind::Transport => StatusCode::BAD_GATEWAY,
            ErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            // 499 mirrors nginx's "client closed request"; not a registered code.
            ErrorKind::Cancelled => StatusCode::from_u16(499).expect("499 is a valid status"),
            ErrorKind::Config | ErrorKind::Storage | ErrorKind::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// Stable machine readable code used in OpenAI/Anthropic error payloads.
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::BadRequest => "invalid_request_error",
            ErrorKind::AuthInvalid => "invalid_api_key",
            ErrorKind::AuthExpired => "credential_expired",
            ErrorKind::AccountDisabled => "account_disabled",
            ErrorKind::ChannelRestricted => "channel_restricted",
            ErrorKind::QuotaExhausted => "insufficient_quota",
            ErrorKind::RateLimited => "rate_limit_exceeded",
            ErrorKind::NoCapacity => "no_capacity",
            ErrorKind::ModelUnavailable => "model_unavailable",
            ErrorKind::SessionBindingInvalid => "session_binding_invalid",
            ErrorKind::Unsupported => "unsupported_capability",
            ErrorKind::UpstreamServer => "upstream_error",
            ErrorKind::Transport => "transport_error",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Cancelled => "request_cancelled",
            ErrorKind::Config => "configuration_error",
            ErrorKind::Storage => "storage_error",
            ErrorKind::Internal => "internal_error",
        }
    }

    fn openai_type(self) -> &'static str {
        match self {
            ErrorKind::BadRequest
            | ErrorKind::ModelUnavailable
            | ErrorKind::SessionBindingInvalid
            | ErrorKind::Unsupported => "invalid_request_error",
            ErrorKind::AuthInvalid | ErrorKind::AuthExpired => "authentication_error",
            ErrorKind::AccountDisabled | ErrorKind::ChannelRestricted => "permission_error",
            ErrorKind::QuotaExhausted => "insufficient_quota",
            ErrorKind::RateLimited => "rate_limit_error",
            ErrorKind::NoCapacity
            | ErrorKind::UpstreamServer
            | ErrorKind::Transport
            | ErrorKind::Timeout
            | ErrorKind::Cancelled
            | ErrorKind::Config
            | ErrorKind::Storage
            | ErrorKind::Internal => "server_error",
        }
    }

    fn anthropic_type(self) -> &'static str {
        match self {
            ErrorKind::BadRequest | ErrorKind::SessionBindingInvalid | ErrorKind::Unsupported => {
                "invalid_request_error"
            }
            ErrorKind::ModelUnavailable => "not_found_error",
            ErrorKind::AuthInvalid | ErrorKind::AuthExpired => "authentication_error",
            ErrorKind::AccountDisabled | ErrorKind::ChannelRestricted => "permission_error",
            ErrorKind::QuotaExhausted | ErrorKind::RateLimited => "rate_limit_error",
            ErrorKind::NoCapacity => "overloaded_error",
            ErrorKind::UpstreamServer
            | ErrorKind::Transport
            | ErrorKind::Timeout
            | ErrorKind::Cancelled
            | ErrorKind::Config
            | ErrorKind::Storage
            | ErrorKind::Internal => "api_error",
        }
    }

    /// Whether the account should be taken out of rotation.
    pub fn blames_account(self) -> bool {
        matches!(
            self,
            ErrorKind::AuthInvalid
                | ErrorKind::AuthExpired
                | ErrorKind::AccountDisabled
                | ErrorKind::ChannelRestricted
        )
    }

    /// Whether the error is transient and retryable on the *same* account.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::RateLimited
                | ErrorKind::UpstreamServer
                | ErrorKind::Transport
                | ErrorKind::Timeout
        )
    }
}

/// Coarse failure domain. Used for scheduling, metrics and admin grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// The downstream client sent an invalid request.
    Request,
    /// Upstream credential/account authorization failure.
    Auth,
    /// Quota or rate limit.
    Quota,
    /// Model selection, session binding or capacity.
    Routing,
    /// Upstream provider failure (server response or transport).
    Provider,
    /// No answer in time.
    Timeout,
    /// Capability not implemented by the provider.
    Unsupported,
    /// Request was cancelled.
    Cancelled,
    /// Local configuration failure.
    Config,
    /// Local database failure.
    Database,
    /// Any other local failure.
    Internal,
}

impl ErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCategory::Request => "request",
            ErrorCategory::Auth => "auth",
            ErrorCategory::Quota => "quota",
            ErrorCategory::Routing => "routing",
            ErrorCategory::Provider => "provider",
            ErrorCategory::Timeout => "timeout",
            ErrorCategory::Unsupported => "unsupported",
            ErrorCategory::Cancelled => "cancelled",
            ErrorCategory::Config => "config",
            ErrorCategory::Database => "database",
            ErrorCategory::Internal => "internal",
        }
    }
}

/// Gateway error with a public message (safe to expose) and optional detail.
///
/// `detail` is diagnostic only; [`public_view`](GatewayError::public_view), the
/// OpenAI/Anthropic bodies and [`GatewayError::log`] are the only consumers, and
/// all of them redact it first.
#[derive(Debug, Clone)]
pub struct GatewayError {
    pub kind: ErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub retry_after: Option<u64>,
    pub upstream_status: Option<u16>,
}

impl GatewayError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let message: String = message.into();
        Self {
            kind,
            message: redact_secrets(&message),
            detail: None,
            retry_after: None,
            upstream_status: None,
        }
    }
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail: String = detail.into();
        self.detail = Some(redact_secrets(&detail));
        self
    }
    pub fn with_upstream_status(mut self, status: u16) -> Self {
        self.upstream_status = Some(status);
        self
    }
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, msg)
    }
    pub fn auth_invalid(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::AuthInvalid, msg)
    }
    pub fn model_unavailable(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::ModelUnavailable, msg)
    }
    pub fn session_binding_invalid(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::SessionBindingInvalid, msg)
    }
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, msg)
    }
    pub fn upstream_server(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::UpstreamServer, msg)
    }
    pub fn transport(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Transport, msg)
    }
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, msg)
    }
    pub fn no_capacity(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::NoCapacity, msg)
    }
    pub fn config(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, msg)
    }
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Storage, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, msg)
    }

    /// Client-safe projection: no detail, no upstream status, no headers.
    pub fn public_view(&self) -> PublicErrorView {
        PublicErrorView {
            kind: self.kind,
            category: self.kind.category(),
            code: self.kind.code(),
            message: redact_secrets(&self.message),
            retry_after: self.retry_after,
        }
    }

    /// Emit a structured log line with redacted diagnostic context.
    pub fn log(&self, operation: &str) {
        let detail = self
            .detail
            .as_deref()
            .map(redact_secrets)
            .unwrap_or_default();
        match self.kind.category() {
            ErrorCategory::Config | ErrorCategory::Database | ErrorCategory::Internal => {
                tracing::error!(
                    operation,
                    kind = self.kind.code(),
                    category = self.kind.category().as_str(),
                    upstream_status = self.upstream_status,
                    retry_after = self.retry_after,
                    detail = %detail,
                    "gateway error"
                );
            }
            _ => {
                tracing::warn!(
                    operation,
                    kind = self.kind.code(),
                    category = self.kind.category().as_str(),
                    upstream_status = self.upstream_status,
                    retry_after = self.retry_after,
                    detail = %detail,
                    "gateway error"
                );
            }
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.code(), self.message)
    }
}

impl std::error::Error for GatewayError {}

impl From<sqlx::Error> for GatewayError {
    fn from(e: sqlx::Error) -> Self {
        let err = GatewayError::storage("database error").with_detail(e.to_string());
        err.log("storage");
        err
    }
}

impl From<reqwest::Error> for GatewayError {
    fn from(e: reqwest::Error) -> Self {
        let err = if e.is_timeout() {
            GatewayError::timeout("upstream request timed out")
        } else if e.is_connect() || e.is_request() {
            GatewayError::transport("failed to reach upstream provider")
        } else if let Some(status) = e.status() {
            GatewayError::upstream_server(format!("upstream returned HTTP {}", status.as_u16()))
                .with_upstream_status(status.as_u16())
        } else {
            GatewayError::upstream_server("upstream request failed")
        };
        let err = err.with_detail(e.to_string());
        err.log("upstream_request");
        err
    }
}

// ---------------------------------------------------------------------------
// Downstream error bodies
// ---------------------------------------------------------------------------

/// Client-safe error projection.
#[derive(Debug, Clone, Serialize)]
pub struct PublicErrorView {
    pub kind: ErrorKind,
    pub category: ErrorCategory,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
}

/// OpenAI style error body.
pub fn openai_error_body(err: &GatewayError) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": redact_secrets(&err.message),
            "type": err.kind.openai_type(),
            "code": err.kind.code(),
            "param": serde_json::Value::Null,
        }
    })
}

/// Anthropic style error body.
pub fn anthropic_error_body(err: &GatewayError) -> serde_json::Value {
    serde_json::json!({
        "type": "error",
        "error": {
            "type": err.kind.anthropic_type(),
            "message": redact_secrets(&err.message),
        }
    })
}

// ---------------------------------------------------------------------------
// Secret redaction
// ---------------------------------------------------------------------------

const MASK: &str = "***";

/// Keys whose value must never reach a client or a log line. Header style keys
/// (cookie, authorization, ...) mask the whole line; parameter style keys mask
/// a single value token.
const SENSITIVE_KEYS: &[&str] = &[
    "proxy-authorization",
    "authorization",
    "set-cookie",
    "cookie",
    "x-api-key",
    "api-key",
    "client_secret",
    "access_token",
    "refresh_token",
    "id_token",
    "api_key",
    "apikey",
    "password",
    "passwd",
    "secret",
    "token",
];

fn is_header_key(key: &str) -> bool {
    matches!(
        key,
        "proxy-authorization" | "authorization" | "set-cookie" | "cookie" | "x-api-key" | "api-key"
    )
}

fn is_value_delim(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\r' | b'\n' | b',' | b';' | b'&' | b'"' | b'\'' | b'}' | b']'
    )
}

/// Mask credentials embedded in free text (headers, JSON, query strings).
///
/// Only values that follow a known key and a `:`/`=` separator are masked, so
/// ordinary diagnostic text ("model not found", "token budget") is untouched.
pub fn redact_secrets(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut copied = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some((key, sep)) = sensitive_key_at(&lower, i) {
            let (value_start, value_end) = value_bounds(&lower, bytes, sep, is_header_key(key));
            if value_end > value_start {
                out.push_str(&input[copied..value_start]);
                out.push_str(MASK);
                copied = value_end;
                i = value_end;
                continue;
            }
            i = sep + 1;
            continue;
        }
        i = next_char_boundary(&lower, i);
    }
    out.push_str(&input[copied..]);
    out
}

/// Find a sensitive key starting at `i` followed by an optional closing quote,
/// an optional run of spaces and a `:`/`=` separator. Returns the key and the
/// separator index.
fn sensitive_key_at(lower: &str, i: usize) -> Option<(&'static str, usize)> {
    let bytes = lower.as_bytes();
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    for key in SENSITIVE_KEYS {
        if lower[i..].starts_with(key) {
            let mut j = i + key.len();
            if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                j += 1;
            }
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b':' || bytes[j] == b'=') {
                return Some((key, j));
            }
        }
    }
    None
}

/// Bounds of the value after `sep`, skipping whitespace and an optional quote.
fn value_bounds(lower: &str, bytes: &[u8], sep: usize, header: bool) -> (usize, usize) {
    let len = bytes.len();
    let mut k = sep + 1;
    while k < len && (bytes[k] == b' ' || bytes[k] == b'\t') {
        k += 1;
    }
    if k < len && (bytes[k] == b'"' || bytes[k] == b'\'') {
        k += 1;
    }
    let start = k;
    let mut v = k;
    if lower[v..].starts_with("bearer ") {
        v += "bearer ".len();
        while v < len && (bytes[v] == b' ' || bytes[v] == b'\t') {
            v += 1;
        }
    }
    if header {
        while v < len && bytes[v] != b'\r' && bytes[v] != b'\n' {
            v += 1;
        }
    } else {
        while v < len && !is_value_delim(bytes[v]) {
            v += 1;
        }
    }
    (start, v)
}

fn next_char_boundary(s: &str, mut i: usize) -> usize {
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kind, HTTP status, machine code, OpenAI type, Anthropic type
    const TABLE: &[(ErrorKind, u16, &str, &str, &str)] = &[
        (
            ErrorKind::BadRequest,
            400,
            "invalid_request_error",
            "invalid_request_error",
            "invalid_request_error",
        ),
        (
            ErrorKind::AuthInvalid,
            401,
            "invalid_api_key",
            "authentication_error",
            "authentication_error",
        ),
        (
            ErrorKind::AuthExpired,
            401,
            "credential_expired",
            "authentication_error",
            "authentication_error",
        ),
        (
            ErrorKind::AccountDisabled,
            403,
            "account_disabled",
            "permission_error",
            "permission_error",
        ),
        (
            ErrorKind::ChannelRestricted,
            403,
            "channel_restricted",
            "permission_error",
            "permission_error",
        ),
        (
            ErrorKind::QuotaExhausted,
            402,
            "insufficient_quota",
            "insufficient_quota",
            "rate_limit_error",
        ),
        (
            ErrorKind::RateLimited,
            429,
            "rate_limit_exceeded",
            "rate_limit_error",
            "rate_limit_error",
        ),
        (
            ErrorKind::NoCapacity,
            503,
            "no_capacity",
            "server_error",
            "overloaded_error",
        ),
        (
            ErrorKind::ModelUnavailable,
            404,
            "model_unavailable",
            "invalid_request_error",
            "not_found_error",
        ),
        (
            ErrorKind::SessionBindingInvalid,
            409,
            "session_binding_invalid",
            "invalid_request_error",
            "invalid_request_error",
        ),
        (
            ErrorKind::Unsupported,
            501,
            "unsupported_capability",
            "invalid_request_error",
            "invalid_request_error",
        ),
        (
            ErrorKind::UpstreamServer,
            502,
            "upstream_error",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Transport,
            502,
            "transport_error",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Timeout,
            504,
            "timeout",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Cancelled,
            499,
            "request_cancelled",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Config,
            500,
            "configuration_error",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Storage,
            500,
            "storage_error",
            "server_error",
            "api_error",
        ),
        (
            ErrorKind::Internal,
            500,
            "internal_error",
            "server_error",
            "api_error",
        ),
    ];

    const CATEGORIES: &[(ErrorKind, ErrorCategory)] = &[
        (ErrorKind::BadRequest, ErrorCategory::Request),
        (ErrorKind::AuthInvalid, ErrorCategory::Auth),
        (ErrorKind::AuthExpired, ErrorCategory::Auth),
        (ErrorKind::AccountDisabled, ErrorCategory::Auth),
        (ErrorKind::ChannelRestricted, ErrorCategory::Auth),
        (ErrorKind::QuotaExhausted, ErrorCategory::Quota),
        (ErrorKind::RateLimited, ErrorCategory::Quota),
        (ErrorKind::NoCapacity, ErrorCategory::Routing),
        (ErrorKind::ModelUnavailable, ErrorCategory::Routing),
        (ErrorKind::SessionBindingInvalid, ErrorCategory::Routing),
        (ErrorKind::Unsupported, ErrorCategory::Unsupported),
        (ErrorKind::UpstreamServer, ErrorCategory::Provider),
        (ErrorKind::Transport, ErrorCategory::Provider),
        (ErrorKind::Timeout, ErrorCategory::Timeout),
        (ErrorKind::Cancelled, ErrorCategory::Cancelled),
        (ErrorKind::Config, ErrorCategory::Config),
        (ErrorKind::Storage, ErrorCategory::Database),
        (ErrorKind::Internal, ErrorCategory::Internal),
    ];

    const SERDE_NAMES: &[(ErrorKind, &str)] = &[
        (ErrorKind::BadRequest, "bad_request"),
        (ErrorKind::AuthInvalid, "auth_invalid"),
        (ErrorKind::AuthExpired, "auth_expired"),
        (ErrorKind::AccountDisabled, "account_disabled"),
        (ErrorKind::ChannelRestricted, "channel_restricted"),
        (ErrorKind::QuotaExhausted, "quota_exhausted"),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::NoCapacity, "no_capacity"),
        (ErrorKind::ModelUnavailable, "model_unavailable"),
        (ErrorKind::SessionBindingInvalid, "session_binding_invalid"),
        (ErrorKind::Unsupported, "unsupported"),
        (ErrorKind::UpstreamServer, "upstream_server"),
        (ErrorKind::Transport, "transport"),
        (ErrorKind::Timeout, "timeout"),
        (ErrorKind::Cancelled, "cancelled"),
        (ErrorKind::Config, "config"),
        (ErrorKind::Storage, "storage"),
        (ErrorKind::Internal, "internal"),
    ];

    fn error_of(kind: ErrorKind) -> GatewayError {
        GatewayError::new(kind, "boom")
            .with_detail("upstream said no")
            .retry_after(7)
    }

    #[test]
    fn tables_cover_every_kind() {
        assert_eq!(TABLE.len(), ErrorKind::ALL.len());
        assert_eq!(CATEGORIES.len(), ErrorKind::ALL.len());
        assert_eq!(SERDE_NAMES.len(), ErrorKind::ALL.len());
        for kind in ErrorKind::ALL {
            assert!(TABLE.iter().any(|row| row.0 == *kind), "{kind:?} missing");
            assert!(
                CATEGORIES.iter().any(|row| row.0 == *kind),
                "{kind:?} missing"
            );
            assert!(
                SERDE_NAMES.iter().any(|row| row.0 == *kind),
                "{kind:?} missing"
            );
        }
    }

    #[test]
    fn kind_maps_to_status_code_and_category() {
        for (kind, status, code, _, _) in TABLE {
            assert_eq!(kind.http_status().as_u16(), *status, "{kind:?} status");
            assert_eq!(kind.code(), *code, "{kind:?} code");
        }
        for (kind, category) in CATEGORIES {
            assert_eq!(kind.category(), *category, "{kind:?} category");
        }
    }

    #[test]
    fn openai_body_is_stable_per_kind() {
        for (kind, _, code, openai_type, _) in TABLE {
            let body = openai_error_body(&error_of(*kind));
            assert_eq!(body["error"]["type"], *openai_type, "{kind:?} type");
            assert_eq!(body["error"]["code"], *code, "{kind:?} code");
            assert_eq!(body["error"]["message"], "boom");
            assert!(body["error"]["param"].is_null());
            assert!(body["error"].get("detail").is_none());
            assert!(body.get("detail").is_none());
        }
    }

    #[test]
    fn anthropic_body_is_stable_per_kind() {
        for (kind, _, _, _, anthropic_type) in TABLE {
            let body = anthropic_error_body(&error_of(*kind));
            assert_eq!(body["type"], "error");
            assert_eq!(body["error"]["type"], *anthropic_type, "{kind:?} type");
            assert_eq!(body["error"]["message"], "boom");
            assert!(body["error"].get("detail").is_none());
        }
    }

    #[test]
    fn serde_names_are_stable() {
        for (kind, name) in SERDE_NAMES {
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                serde_json::Value::String((*name).into())
            );
        }
    }

    #[test]
    fn unsupported_maps_to_501() {
        let err = GatewayError::unsupported("agent tasks are not supported");
        assert_eq!(err.kind, ErrorKind::Unsupported);
        assert_eq!(err.kind.http_status().as_u16(), 501);
        assert_eq!(err.kind.category(), ErrorCategory::Unsupported);
        assert_eq!(err.public_view().code, "unsupported_capability");
        assert!(!err.kind.blames_account());
        assert!(!err.kind.retryable());
    }

    #[test]
    fn timeout_is_504_provider_retryable() {
        let err = GatewayError::timeout("no answer");
        assert_eq!(err.kind.http_status().as_u16(), 504);
        assert_eq!(err.kind.category(), ErrorCategory::Timeout);
        assert!(err.kind.retryable());
        assert!(!err.kind.blames_account());
        assert_eq!(err.public_view().message, "no answer");
    }

    #[test]
    fn provider_auth_failures_blame_account() {
        for kind in [
            ErrorKind::AuthInvalid,
            ErrorKind::AuthExpired,
            ErrorKind::AccountDisabled,
            ErrorKind::ChannelRestricted,
        ] {
            assert_eq!(kind.category(), ErrorCategory::Auth, "{kind:?}");
            assert!(kind.blames_account(), "{kind:?}");
            assert!(!kind.retryable(), "{kind:?}");
        }
        for kind in [ErrorKind::BadRequest, ErrorKind::ModelUnavailable] {
            assert!(!kind.blames_account(), "{kind:?}");
        }
    }

    #[test]
    fn client_errors_do_not_blame_account() {
        for kind in [
            ErrorKind::BadRequest,
            ErrorKind::ModelUnavailable,
            ErrorKind::QuotaExhausted,
            ErrorKind::RateLimited,
            ErrorKind::NoCapacity,
            ErrorKind::Unsupported,
        ] {
            assert!(!kind.blames_account(), "{kind:?}");
        }
        assert!(ErrorKind::RateLimited.retryable());
        assert!(!ErrorKind::QuotaExhausted.retryable());
    }

    #[test]
    fn public_view_hides_detail_and_upstream_status() {
        let err = GatewayError::new(ErrorKind::UpstreamServer, "upstream failed")
            .with_detail("secret-ish upstream payload")
            .with_upstream_status(500)
            .retry_after(3);
        let view = err.public_view();
        assert_eq!(view.kind, ErrorKind::UpstreamServer);
        assert_eq!(view.category, ErrorCategory::Provider);
        assert_eq!(view.code, "upstream_error");
        assert_eq!(view.retry_after, Some(3));
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(!json.contains("detail"));
        assert!(!json.contains("upstream_status"));
        assert!(!json.contains("500"));
    }

    #[test]
    fn redacts_authorization_cookie_and_api_key_headers() {
        let raw = "Authorization: Bearer sk-live-abc123\nCookie: sid=deadbeef; theme=dark\nX-Api-Key: key-42\nSet-Cookie: session=zzz";
        let out = redact_secrets(raw);
        for secret in ["sk-live-abc123", "deadbeef", "theme=dark", "key-42", "zzz"] {
            assert!(!out.contains(secret), "leaked {secret} in {out}");
        }
        assert!(out.contains("Authorization: ***"));
        assert!(out.contains("Cookie: ***"));
        assert!(out.contains("X-Api-Key: ***"));
        assert!(out.contains("Set-Cookie: ***"));
    }

    #[test]
    fn redacts_query_and_json_credentials_only() {
        let url = "GET https://host/v1?access_token=abc123&page=2";
        let out = redact_secrets(url);
        assert!(!out.contains("abc123"));
        assert!(out.contains("access_token=***&page=2"));

        let json = r#"{"refresh_token":"rt-9","expires_in":3600,"api_key":"k-1"}"#;
        let out = redact_secrets(json);
        assert!(!out.contains("rt-9"));
        assert!(!out.contains("k-1"));
        assert!(out.contains(r#""refresh_token":"***""#));
        assert!(out.contains(r#""api_key":"***""#));
        assert!(out.contains("\"expires_in\":3600"));
    }

    #[test]
    fn redaction_leaves_ordinary_text_intact() {
        for text in [
            "model not found",
            "token budget exceeded",
            "the tokenizer failed",
            "credential is missing required field `token`",
        ] {
            assert_eq!(redact_secrets(text), text);
        }
    }

    #[test]
    fn message_and_detail_are_redacted_at_construction() {
        let err = GatewayError::bad_request("bad header Cookie: sid=deadbeef")
            .with_detail("call failed: Authorization: Bearer sk-live-abc");
        assert!(!err.message.contains("deadbeef"));
        assert!(err.message.contains("Cookie: ***"));
        let detail = err.detail.as_deref().unwrap_or_default();
        assert!(!detail.contains("sk-live-abc"));
        assert!(detail.contains("Authorization: ***"));
        let body = openai_error_body(&err);
        assert!(!body.to_string().contains("deadbeef"));
    }
}
