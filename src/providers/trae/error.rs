//! TRAE upstream error classification.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §11, built from
//! `Coding2API/src/provider/trae/events.py:233-289` (`classify_error_code`,
//! `classify_status`) and `dsh-wtq-pool/src/trae-upstream.ts:305-320`
//! (`classifyTraeUpstreamError`). TRAE reports most failures as an HTTP status,
//! but in-stream failures arrive as an `event: error` frame carrying a business
//! code, and some 2xx responses carry `{code, message}`.
//!
//! Rules that matter for account safety:
//! * `402`, `14018` and `4008` are credits/quota; they never mark the
//!   credential dead;
//! * `1005` is a plan/entitlement restriction (channel restricted), again not a
//!   dead token;
//! * `401` is the only status mapped to an expired credential;
//! * `404` is the reference's "soft" failure, mapped to rate limiting rather
//!   than to a dead account;
//! * `4001` is a bad request/model parameter; switching accounts cannot fix it.

use serde_json::Value;

use crate::error::{redact_secrets, ErrorKind, GatewayError};

const MAX_MESSAGE_CHARS: usize = 512;

pub fn classify_http_status(status: u16, body: &str) -> GatewayError {
    let codes = business_codes(body);
    let kind = kind_for(i64::from(status), &codes);
    let message = public_message(i64::from(status), &codes, body);
    GatewayError::new(kind, message).with_upstream_status(status)
}

/// Classify an `event: error` frame, which has a business code but no HTTP
/// status.
pub fn classify_stream_error(code: Option<i64>, message: Option<&str>) -> GatewayError {
    let kind = match code {
        Some(1005) => ErrorKind::ChannelRestricted,
        Some(14018) | Some(4008) => ErrorKind::QuotaExhausted,
        Some(6004) => ErrorKind::ModelUnavailable,
        Some(4001) => ErrorKind::BadRequest,
        _ => ErrorKind::UpstreamServer,
    };
    let label = match code {
        Some(code) => format!("TRAE code {code}"),
        None => "TRAE stream error".to_string(),
    };
    let message = message.map(str::trim).filter(|text| !text.is_empty());
    let text = match message {
        Some(message) => format!("{label}: {}", truncate(message)),
        None => label,
    };
    GatewayError::new(kind, text)
}

pub fn classify_transport(error: &reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        return GatewayError::new(ErrorKind::Timeout, "TRAE request timed out");
    }
    GatewayError::new(
        ErrorKind::Transport,
        format!("TRAE transport failure: {error}"),
    )
}

fn kind_for(status: i64, codes: &[i64]) -> ErrorKind {
    if status == 402 || codes.contains(&14018) || codes.contains(&4008) {
        return ErrorKind::QuotaExhausted;
    }
    if codes.contains(&1005) {
        return ErrorKind::ChannelRestricted;
    }
    if (status == 400 || status == 404) && codes.contains(&11102) {
        return ErrorKind::AccountDisabled;
    }
    match status {
        401 => ErrorKind::AuthExpired,
        404 => ErrorKind::RateLimited,
        429 => {
            if codes.contains(&6004) {
                ErrorKind::ModelUnavailable
            } else {
                ErrorKind::RateLimited
            }
        }
        400 => ErrorKind::BadRequest,
        408 | 425 => ErrorKind::Timeout,
        500..=599 => ErrorKind::UpstreamServer,
        _ => ErrorKind::UpstreamServer,
    }
}

fn public_message(status: i64, codes: &[i64], body: &str) -> String {
    let message = envelope_message(body);
    let label = match codes.first() {
        Some(code) => format!("TRAE code {code}"),
        None => format!("TRAE HTTP {status}"),
    };
    match message {
        Some(message) => format!("{label}: {}", truncate(&message)),
        None => label,
    }
}

fn truncate(text: &str) -> String {
    let redacted = redact_secrets(text.trim());
    redacted.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// Whether a 2xx body still carries a non-zero business code. Used by the JSON
/// endpoints, where a failure may arrive as HTTP 200.
pub fn is_business_failure(body: &str) -> bool {
    !business_codes(body).is_empty()
}

/// Collect business codes from the envelope shapes the reference observes:
/// top-level `code`, `error.code` and `data.code`. Numeric strings are accepted
/// because some TRAE builds quote the code.
fn business_codes(body: &str) -> Vec<i64> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut codes = Vec::new();
    for pointer in ["/code", "/error/code", "/data/code"] {
        let Some(value) = value.pointer(pointer) else {
            continue;
        };
        let code = match value {
            Value::Number(number) => number.as_i64(),
            Value::String(text) => text.trim().parse::<i64>().ok(),
            _ => None,
        };
        if let Some(code) = code.filter(|code| *code != 0) {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
    }
    codes
}

fn envelope_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    ["/message", "/msg", "/error/message", "/data/message"]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_quota_are_distinct() {
        assert_eq!(classify_http_status(401, "{}").kind, ErrorKind::AuthExpired);
        assert_eq!(
            classify_http_status(402, r#"{"code":14018,"message":"credits"}"#).kind,
            ErrorKind::QuotaExhausted
        );
        assert_eq!(
            classify_http_status(200, r#"{"code":4008,"message":"quota"}"#).kind,
            ErrorKind::QuotaExhausted
        );
    }

    #[test]
    fn plan_restriction_is_not_a_dead_account() {
        let error = classify_http_status(400, r#"{"code":1005,"message":"plan required"}"#);
        assert_eq!(error.kind, ErrorKind::ChannelRestricted);
        assert_eq!(error.upstream_status, Some(400));
    }

    #[test]
    fn model_and_request_failures_keep_their_meaning() {
        assert_eq!(
            classify_http_status(429, r#"{"code":6004}"#).kind,
            ErrorKind::ModelUnavailable
        );
        assert_eq!(classify_http_status(429, "{}").kind, ErrorKind::RateLimited);
        assert_eq!(classify_http_status(400, "{}").kind, ErrorKind::BadRequest);
        assert_eq!(classify_http_status(404, "{}").kind, ErrorKind::RateLimited);
    }

    #[test]
    fn blocked_account_is_recognised() {
        assert_eq!(
            classify_http_status(400, r#"{"code":11102}"#).kind,
            ErrorKind::AccountDisabled
        );
    }

    #[test]
    fn stream_codes_match_the_http_mapping() {
        assert_eq!(
            classify_stream_error(Some(1005), None).kind,
            ErrorKind::ChannelRestricted
        );
        assert_eq!(
            classify_stream_error(Some(4008), Some("quota")).kind,
            ErrorKind::QuotaExhausted
        );
        assert_eq!(
            classify_stream_error(Some(4001), None).kind,
            ErrorKind::BadRequest
        );
        assert_eq!(
            classify_stream_error(None, Some("boom")).kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn business_failure_scans_nested_codes() {
        assert!(!is_business_failure("{}"));
        assert!(!is_business_failure(r#"{"code":0}"#));
        assert!(is_business_failure(r#"{"error":{"code":6004}}"#));
        assert!(is_business_failure(r#"{"data":{"code":"4001"}}"#));
    }

    #[test]
    fn token_in_message_is_redacted() {
        let error = classify_http_status(
            400,
            r#"{"code":4001,"message":"bad access_token=eyJhbGciOiJIUzI1NiJ9abcdefgh"}"#,
        );
        assert!(!error.message.contains("eyJhbGciOiJIUzI1NiJ9abcdefgh"));
    }
}
