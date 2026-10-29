//! WorkBuddy upstream error classification.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §12.2, which is built from
//! `workbuddy2api/docs/protocol-reference.md:1840-1950` (error table),
//! `Coding2API/src/provider/codebuddy/events.py:55-150` and
//! `dsh/src/upstream.ts:440-475`.
//!
//! Rules that matter for account safety:
//! * business codes win over HTTP status: the upstream puts failures in a JSON
//!   envelope on 200 and 400 alike;
//! * `402`/`412` and the `140xx` family are quota, never a dead credential;
//! * `11140` is an account restriction; it must stop dispatch for that account
//!   and must not be retried on another one automatically;
//! * `11128` is request-level. Two different checks share the code, so it is
//!   reported as an upstream failure without blaming the account;
//! * `6004`/`6008` and the `6000`/`6020` families are rate limits.

use serde_json::Value;

use crate::error::{redact_secrets, ErrorKind, GatewayError};

const MAX_MESSAGE_CHARS: usize = 512;
const MAX_RETRY_AFTER_SECONDS: u64 = 3_600;

pub fn classify_http_status(status: u16, body: &str) -> GatewayError {
    classify_status(i64::from(status), body)
}

pub fn classify_status(status: i64, body: &str) -> GatewayError {
    let payload: Option<Value> = serde_json::from_str(body).ok();
    if let Some(payload) = payload.as_ref() {
        if let Some(error) = classify_payload(payload) {
            return with_status(error, status);
        }
    }
    let lowered = body.to_ascii_lowercase();
    let error = match status {
        401 => GatewayError::new(
            ErrorKind::AuthExpired,
            "WorkBuddy rejected the credential; refresh or re-authorize the account",
        ),
        402 | 412 => GatewayError::new(ErrorKind::QuotaExhausted, "WorkBuddy credits are used up"),
        403 => GatewayError::new(
            ErrorKind::UpstreamServer,
            "WorkBuddy refused the request (no business code; possible channel check)",
        ),
        408 | 425 => GatewayError::new(ErrorKind::Timeout, "WorkBuddy did not answer in time"),
        429 => GatewayError::new(ErrorKind::RateLimited, "WorkBuddy rate limit"),
        500..=599 => GatewayError::new(ErrorKind::UpstreamServer, "WorkBuddy upstream failure"),
        _ if lowered.contains("offline user session not found") => GatewayError::new(
            ErrorKind::AuthExpired,
            "WorkBuddy session is no longer valid; re-authorize the account",
        ),
        _ => GatewayError::new(
            ErrorKind::UpstreamServer,
            format!("WorkBuddy returned HTTP {status}"),
        ),
    };
    let error = with_status(error, status);
    match retry_after_from_body(body) {
        Some(seconds) => error.retry_after(seconds),
        None => error,
    }
}

/// Classify a JSON envelope. Returns `None` for `code == 0` or an envelope with
/// no code at all.
pub fn classify_payload(value: &Value) -> Option<GatewayError> {
    let code = business_code(value)?;
    if code == 0 {
        return None;
    }
    let message = envelope_message(value).unwrap_or_else(|| format!("WorkBuddy code {code}"));
    let error = classify_code(code, &message);
    match business_retry_after(value) {
        Some(seconds) => Some(error.retry_after(seconds)),
        None => Some(error),
    }
}

/// Used for an error frame inside an SSE stream, where there is no HTTP status.
pub fn classify_envelope(code: i64, body: &str) -> GatewayError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| envelope_message(&value))
        .unwrap_or_else(|| format!("WorkBuddy code {code}"));
    classify_code(code, &message)
}

pub fn classify_transport(error: &reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        return GatewayError::new(ErrorKind::Timeout, "WorkBuddy request timed out");
    }
    GatewayError::new(
        ErrorKind::Transport,
        format!("WorkBuddy transport failure: {error}"),
    )
}

fn classify_code(code: i64, message: &str) -> GatewayError {
    let kind = match code {
        401 | 12153 => ErrorKind::AuthExpired,
        11140 => ErrorKind::ChannelRestricted,
        1005 => ErrorKind::ChannelRestricted,
        402 | 412 | 14001 | 14012 | 14013 | 14014 | 14018 => ErrorKind::QuotaExhausted,
        429 | 14003 => ErrorKind::RateLimited,
        6004 | 6008 | 6020..=6022 | 6000..=6003 | 6005..=6007 => ErrorKind::RateLimited,
        10105 => ErrorKind::RateLimited,
        11102 | 11158 => ErrorKind::ModelUnavailable,
        11101 | 11115 | 11129 | 10012 | 11148 | 11149 => ErrorKind::BadRequest,
        11128 | 11135 => ErrorKind::UpstreamServer,
        _ => ErrorKind::UpstreamServer,
    };
    GatewayError::new(kind, public_message(code, message))
}

fn with_status(error: GatewayError, status: i64) -> GatewayError {
    if (0..=u16::MAX as i64).contains(&status) {
        error.with_upstream_status(status as u16)
    } else {
        error
    }
}

fn public_message(code: i64, message: &str) -> String {
    let redacted = redact_secrets(message);
    let text: String = redacted.chars().take(MAX_MESSAGE_CHARS).collect();
    if text.trim().is_empty() {
        format!("WorkBuddy code {code}")
    } else {
        format!("WorkBuddy code {code}: {text}")
    }
}

fn business_code(value: &Value) -> Option<i64> {
    let code = value
        .get("code")
        .or_else(|| value.get("error").and_then(|error| error.get("code")))?;
    match code {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn envelope_message(value: &Value) -> Option<String> {
    value
        .get("msg")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error").and_then(|error| error.get("message")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn retry_after_from_body(body: &str) -> Option<u64> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| business_retry_after(&value))
}

fn business_retry_after(value: &Value) -> Option<u64> {
    for key in ["retry_after", "retryAfter", "retryAfterSeconds"] {
        if let Some(seconds) = value.get(key).and_then(value_as_seconds) {
            return Some(seconds);
        }
    }
    value
        .get("data")
        .and_then(|data| data.get("retry_after"))
        .and_then(value_as_seconds)
}

fn value_as_seconds(value: &Value) -> Option<u64> {
    let number = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| *number > 0.0)
            .map(|number| number as u64)
    })?;
    // Queue codes report milliseconds in some builds.
    let seconds = if number > MAX_RETRY_AFTER_SECONDS * 10 {
        number / 1000
    } else {
        number
    };
    Some(seconds.clamp(1, MAX_RETRY_AFTER_SECONDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_quota_and_restriction_are_distinct() {
        assert_eq!(
            classify_http_status(401, r#"{"code":401,"msg":"expired"}"#).kind,
            ErrorKind::AuthExpired
        );
        assert_eq!(
            classify_http_status(402, r#"{"code":14018,"msg":"credits"}"#).kind,
            ErrorKind::QuotaExhausted
        );
        assert_eq!(
            classify_http_status(403, r#"{"code":11140,"msg":"restricted"}"#).kind,
            ErrorKind::ChannelRestricted
        );
    }

    #[test]
    fn business_code_wins_over_http_200() {
        let error = classify_http_status(200, r#"{"code":6004,"msg":"daily"}"#);
        assert_eq!(error.kind, ErrorKind::RateLimited);
    }

    #[test]
    fn queue_codes_carry_retry_after() {
        let error = classify_http_status(429, r#"{"code":6020,"msg":"queued","retryAfter":3}"#);
        assert_eq!(error.kind, ErrorKind::RateLimited);
        assert_eq!(error.retry_after, Some(3));
    }

    #[test]
    fn channel_check_is_request_level_not_account_level() {
        let error = classify_http_status(
            400,
            r#"{"code":11128,"msg":"Illegal API invocation from an unapproved channel"}"#,
        );
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }

    #[test]
    fn five_hundred_is_upstream_server() {
        assert_eq!(
            classify_http_status(503, "gateway down").kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn token_in_message_is_redacted() {
        let error = classify_http_status(
            400,
            r#"{"code":11101,"msg":"bad access_token=eyJhbGciOiJIUzI1NiJ9abcdefgh"}"#,
        );
        assert!(!error.message.contains("eyJhbGciOiJIUzI1NiJ9abcdefgh"));
    }
}
