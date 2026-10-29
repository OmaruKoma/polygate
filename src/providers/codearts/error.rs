//! CodeArts upstream error classification.
//!
//! Only markers that appear in the reference sources are special-cased:
//!
//! * `TM.00001041` / `并发` — the account's three upstream sessions are busy
//!   (observed as HTTP 400, not 429).
//! * `APIG.0602` / `security token has expired` — the temporary STS credential
//!   expired; recoverable by refresh.
//! * `APIG.0301` / `decrypt token fail` — the credential/signature is rejected.
//! * `002002009` / `not registered`, `4004.200` / `benefit not found` — the model
//!   is not usable for this account.
//!
//! Everything else falls back to the HTTP status. Upstream response bodies are
//! redacted and truncated before they can reach a log line or a client.

use chrono::{DateTime, Utc};

use crate::error::{redact_secrets, ErrorKind, GatewayError};

const MAX_MESSAGE_CHARS: usize = 512;

const CONCURRENCY_MARKERS: &[&str] = &["00001041", "并发"];
const MODEL_ABSENT_MARKERS: &[&str] = &[
    "002002009",
    "not registered",
    "4004.200",
    "benefit not found",
];
const STS_EXPIRED_MARKERS: &[&str] = &["apig.0602", "security token has expired"];
const REQUEST_MARKERS: &[&str] = &["001001020", "not multimodal"];
const QUOTA_MARKERS: &[&str] = &["1005", "quota", "insufficient"];

fn public_message(prefix: &str, body: &str) -> String {
    let redacted = redact_secrets(body);
    let mut text: String = redacted.chars().take(MAX_MESSAGE_CHARS).collect();
    if redacted.chars().count() > MAX_MESSAGE_CHARS {
        text.push('…');
    }
    if text.trim().is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {text}")
    }
}

/// Classify a non-2xx HTTP response.
pub fn classify_http_status(status: u16, body: &str) -> GatewayError {
    let lowered = body.to_ascii_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|marker| lowered.contains(marker));

    if status == 402 || has(QUOTA_MARKERS) {
        return GatewayError::new(
            ErrorKind::QuotaExhausted,
            public_message("CodeArts quota exhausted", body),
        );
    }
    if has(STS_EXPIRED_MARKERS) {
        return GatewayError::new(
            ErrorKind::AuthExpired,
            public_message("CodeArts security token expired", body),
        );
    }
    if status == 401 || status == 403 || lowered.contains("apig.0301") {
        return GatewayError::new(
            ErrorKind::AuthInvalid,
            public_message("CodeArts rejected the credential", body),
        );
    }
    if (status == 400 || status == 404) && has(MODEL_ABSENT_MARKERS) {
        return GatewayError::new(
            ErrorKind::ModelUnavailable,
            public_message("CodeArts model is not available for this account", body),
        );
    }
    if (status == 400 || status == 429) && has(CONCURRENCY_MARKERS) {
        return GatewayError::new(
            ErrorKind::RateLimited,
            public_message("CodeArts upstream session limit reached", body),
        );
    }
    if (status == 400 || status == 429) && has(&["tpm", "rate limit", "throttl"]) {
        return GatewayError::new(
            ErrorKind::RateLimited,
            public_message("CodeArts rate limited the request", body),
        );
    }
    if (status == 400 || status == 404) && has(REQUEST_MARKERS) {
        return GatewayError::new(
            ErrorKind::BadRequest,
            public_message("CodeArts rejected the request shape", body),
        );
    }
    if status == 429 {
        return GatewayError::new(
            ErrorKind::RateLimited,
            public_message("CodeArts rate limited the request", body),
        );
    }
    if (500..=599).contains(&status) {
        return GatewayError::new(
            ErrorKind::UpstreamServer,
            public_message(&format!("CodeArts upstream HTTP {status}"), body),
        );
    }
    if (400..500).contains(&status) {
        return GatewayError::new(
            ErrorKind::BadRequest,
            public_message(&format!("CodeArts upstream HTTP {status}"), body),
        );
    }
    GatewayError::new(
        ErrorKind::UpstreamServer,
        public_message("CodeArts upstream error", body),
    )
}

/// Classify an error embedded in an HTTP 200 SSE frame.
pub fn classify_stream_error(code: &str, message: &str) -> GatewayError {
    let text = format!("{code} {message}").to_ascii_lowercase();
    let has = |markers: &[&str]| markers.iter().any(|marker| text.contains(marker));

    if has(CONCURRENCY_MARKERS) {
        return GatewayError::new(
            ErrorKind::RateLimited,
            public_message("CodeArts upstream session limit reached", &text),
        );
    }
    if has(STS_EXPIRED_MARKERS) {
        return GatewayError::new(
            ErrorKind::AuthExpired,
            public_message("CodeArts security token expired", &text),
        );
    }
    if text.contains("apig.0301") {
        return GatewayError::new(
            ErrorKind::AuthInvalid,
            public_message("CodeArts rejected the credential", &text),
        );
    }
    if has(MODEL_ABSENT_MARKERS) {
        return GatewayError::new(
            ErrorKind::ModelUnavailable,
            public_message("CodeArts model is not available for this account", &text),
        );
    }
    if has(REQUEST_MARKERS) {
        return GatewayError::new(
            ErrorKind::BadRequest,
            public_message("CodeArts rejected the request shape", &text),
        );
    }
    if has(QUOTA_MARKERS) {
        return GatewayError::new(
            ErrorKind::QuotaExhausted,
            public_message("CodeArts quota exhausted", &text),
        );
    }
    GatewayError::new(
        ErrorKind::UpstreamServer,
        public_message("CodeArts stream error", &text),
    )
}

/// `Retry-After` in seconds, when the upstream sent one.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or_else(|| {
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| DateTime::parse_from_rfc2822(value.trim()).ok())
                .map(|when| (when.with_timezone(&Utc) - Utc::now()).num_seconds().max(0) as u64)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_limit_is_retryable_even_on_http_400() {
        let err = classify_http_status(
            400,
            r#"{"error_code":"TM.00001041","error_msg":"并发会话数已达上限(3个)"}"#,
        );
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert!(err.message.contains("session limit"));
    }

    #[test]
    fn sts_expiry_is_recoverable() {
        let err = classify_http_status(400, "APIG.0602 The security token has expired.");
        assert_eq!(err.kind, ErrorKind::AuthExpired);
    }

    #[test]
    fn model_absence_is_not_a_rate_limit() {
        let err = classify_http_status(404, "InferHub.002002009.404 model is not registered");
        assert_eq!(err.kind, ErrorKind::ModelUnavailable);
    }

    #[test]
    fn server_errors_and_transport_are_distinct() {
        assert_eq!(
            classify_http_status(503, "busy").kind,
            ErrorKind::UpstreamServer
        );
        assert_eq!(classify_http_status(429, "").kind, ErrorKind::RateLimited);
        assert_eq!(
            classify_http_status(401, "unauthorized").kind,
            ErrorKind::AuthInvalid
        );
    }

    #[test]
    fn stream_error_codes_are_classified() {
        assert_eq!(
            classify_stream_error("TM.00001041", "并发会话数已达上限").kind,
            ErrorKind::RateLimited
        );
        assert_eq!(
            classify_stream_error("ChatAgent.00001001", "failed").kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn body_secrets_are_redacted_and_truncated() {
        let body = format!(
            "{{\"x-auth-token\":\"super-secret-value\",\"pad\":\"{}\"}}",
            "a".repeat(2000)
        );
        let err = classify_http_status(400, &body);
        assert!(!err.message.contains("super-secret-value"));
        assert!(err.message.chars().count() < 600);
    }
}
