//! Qoder upstream error classification.
//!
//! Source: Coding2API `src/provider/qoder/events.py` (`classify_error_code`,
//! `classify_status`, marker constants) plus `src/provider/base.py`
//! (`parse_queue_hint`) at commit `df9936240708f99f5a1cd92e52330b4b2016c840`.
//!
//! Rules that matter for account safety:
//! * `402` or "Billing daily count exceeded" is a quota condition, not a dead
//!   credential.
//! * A queue marker is a rate limit, not "session dead".
//! * `401` is the only unconditional authentication failure.
//! * `403` is only a credential failure when the body carries a session marker;
//!   otherwise it is treated as a transient upstream error (the reference
//!   implementation warns that treating every 403 as dead bans healthy
//!   accounts).
//! * A `400` carrying a node-failure marker is a model-level transient error,
//!   not an invalid request.
//!
//! Device authorization reports "still waiting" as `202`/`404`; that is not an
//! inference error and is classified separately by [`classify_device_poll`].

use serde_json::Value;

use crate::error::{redact_secrets, ErrorKind, GatewayError};

const MAX_MESSAGE_CHARS: usize = 512;

const DAILY_QUOTA_MARKERS: &[&str] = &["billing daily count exceeded"];
const SESSION_EXPIRED_MARKERS: &[&str] = &["token_expire"];
const SESSION_DEAD_MARKERS: &[&str] = &["12153", "offline user session not found"];
const PERMISSION_MARKERS: &[&str] = &[
    "permission denied",
    "not authorized",
    "access denied",
    "no permission",
];
const MODEL_MARKERS: &[&str] = &[
    "model not found",
    "model_not_found",
    "model not registered",
    "model is not available",
    "model unavailable",
    "unknown model",
    "no such model",
    "model not supported",
    "unsupported model",
];
const NODE_FAILURE_MARKERS: &[&str] = &["[fail]node:", "execution failed"];

const QUEUE_FLAG_KEYS: &[&str] = &["isQueued", "is_queued", "queued"];
const QUEUE_WAIT_KEYS: &[&str] = &[
    "retryAfterSeconds",
    "retry_after_seconds",
    "retryAfter",
    "retry_after",
];
const QUEUE_SUB_CODES: &[&str] = &["resource_limit", "resource_limit_exceeded"];
const QUEUE_DEFAULT_WAIT_SECONDS: u64 = 5;
const QUEUE_MAX_WAIT_SECONDS: u64 = 60;
const QUEUE_MAX_DEPTH: u8 = 3;
const MAX_QUEUE_BODY_BYTES: usize = 64 * 1024;

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

fn contains_any(lowered: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| lowered.contains(marker))
}

/// Classify a non-2xx HTTP response.
pub fn classify_http_status(status: u16, body: &str) -> GatewayError {
    classify_status(i64::from(status), body)
}

/// Classify an HTTP status or an SSE envelope `statusCodeValue`.
pub fn classify_status(status: i64, body: &str) -> GatewayError {
    let lowered = body.to_ascii_lowercase();
    let with_status = |error: GatewayError| {
        if (0..=u16::MAX as i64).contains(&status) {
            error.with_upstream_status(status as u16)
        } else {
            error
        }
    };

    if contains_any(&lowered, DAILY_QUOTA_MARKERS) {
        return with_status(GatewayError::new(
            ErrorKind::QuotaExhausted,
            public_message("Qoder daily billing limit reached", body),
        ));
    }
    if let Some(wait) = queue_hint(body) {
        return with_status(
            GatewayError::new(
                ErrorKind::RateLimited,
                public_message("Qoder upstream is queueing requests", body),
            )
            .retry_after(wait),
        );
    }
    if status == 402 {
        return with_status(GatewayError::new(
            ErrorKind::QuotaExhausted,
            public_message("Qoder quota exhausted", body),
        ));
    }
    if status == 401 {
        return with_status(GatewayError::new(
            ErrorKind::AuthInvalid,
            public_message("Qoder rejected the credential", body),
        ));
    }
    if status == 403 {
        if contains_any(&lowered, SESSION_EXPIRED_MARKERS) {
            return with_status(GatewayError::new(
                ErrorKind::AuthExpired,
                public_message("Qoder session expired", body),
            ));
        }
        if contains_any(&lowered, SESSION_DEAD_MARKERS) {
            return with_status(GatewayError::new(
                ErrorKind::AuthInvalid,
                public_message("Qoder session is no longer valid", body),
            ));
        }
        if contains_any(&lowered, PERMISSION_MARKERS) {
            return with_status(GatewayError::new(
                ErrorKind::ChannelRestricted,
                public_message("Qoder denied permission for this account", body),
            ));
        }
        // Transient upstream/risk-control rejection. It must not disable the
        // account; the reference implementation treats this as a soft failure.
        return with_status(GatewayError::new(
            ErrorKind::UpstreamServer,
            public_message("Qoder rejected the request", body),
        ));
    }
    if status == 429 {
        return with_status(GatewayError::new(
            ErrorKind::RateLimited,
            public_message("Qoder rate limited the request", body),
        ));
    }
    if status == 408 || status == 504 {
        return with_status(GatewayError::new(
            ErrorKind::Timeout,
            public_message("Qoder upstream timed out", body),
        ));
    }
    if (500..=599).contains(&status) || status == 418 {
        return with_status(GatewayError::new(
            ErrorKind::UpstreamServer,
            public_message("Qoder upstream is unavailable", body),
        ));
    }
    if status == 400 || status == 404 || status == 422 {
        if contains_any(&lowered, NODE_FAILURE_MARKERS) {
            return with_status(GatewayError::new(
                ErrorKind::UpstreamServer,
                public_message("Qoder model node failed", body),
            ));
        }
        if contains_any(&lowered, MODEL_MARKERS) {
            return with_status(GatewayError::new(
                ErrorKind::ModelUnavailable,
                public_message("Qoder model is not available", body),
            ));
        }
        return with_status(GatewayError::new(
            ErrorKind::BadRequest,
            public_message("Qoder rejected the request shape", body),
        ));
    }
    if (400..500).contains(&status) {
        return with_status(GatewayError::new(
            ErrorKind::BadRequest,
            public_message("Qoder rejected the request", body),
        ));
    }
    with_status(GatewayError::new(
        ErrorKind::UpstreamServer,
        public_message("Qoder returned an unexpected status", body),
    ))
}

/// Classify an error carried by an SSE envelope. `status` is
/// `statusCodeValue` (200 for a malformed 200 envelope).
pub fn classify_envelope(status: i64, body: &str) -> GatewayError {
    if status != 200 {
        return classify_status(status, body);
    }
    if let Ok(payload) = serde_json::from_str::<Value>(body) {
        if let Some(error) = classify_payload(&payload) {
            return error;
        }
    }
    GatewayError::new(
        ErrorKind::UpstreamServer,
        public_message("Qoder stream reported an error", body),
    )
}

/// Detect an error object inside a JSON payload (inner chunk, HTTP body or an
/// envelope's serialized body).
pub fn classify_payload(payload: &Value) -> Option<GatewayError> {
    if let Some(status) = payload.get("statusCodeValue") {
        let status = status_code(status);
        if status != 200 {
            return Some(classify_envelope(status, &payload.to_string()));
        }
    }
    if let Some(error) = payload.get("error") {
        let (code, message) = error_parts(error);
        return Some(match code {
            Some(code) => classify_status(code, &payload.to_string()),
            None => GatewayError::new(
                ErrorKind::UpstreamServer,
                public_message("Qoder upstream error", &message),
            ),
        });
    }
    if let Some(code) = payload.get("code").and_then(code_value) {
        if code != 0 && code != 200 {
            return Some(classify_status(code, &payload.to_string()));
        }
    }
    if payload.get("success").and_then(Value::as_bool) == Some(false) {
        let message = payload
            .get("message")
            .or_else(|| payload.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("Qoder upstream reported a failure");
        return Some(GatewayError::new(
            ErrorKind::UpstreamServer,
            public_message("Qoder upstream error", message),
        ));
    }
    None
}

/// Device-code polling is not an inference call. `202` and `404` mean "the user
/// has not finished authorization yet" and must not become an error.
#[derive(Debug)]
pub enum DevicePollOutcome {
    Pending,
    Ready,
    Failed(GatewayError),
}

pub fn classify_device_poll(status: u16, body: &str) -> DevicePollOutcome {
    match status {
        202 | 404 => DevicePollOutcome::Pending,
        200 => DevicePollOutcome::Ready,
        _ => DevicePollOutcome::Failed(classify_http_status(status, body)),
    }
}

/// Map a transport failure. Connection resets are retryable transport errors;
/// timeouts are distinct so the caller can apply its own deadline policy.
pub fn classify_transport(error: &reqwest::Error) -> GatewayError {
    if error.is_timeout() {
        return GatewayError::timeout("Qoder upstream timed out");
    }
    if error.is_connect() {
        return GatewayError::transport("Qoder upstream is unreachable");
    }
    GatewayError::transport("Qoder transport failed").with_detail(error.to_string())
}

fn status_code(value: &Value) -> i64 {
    if value.is_boolean() {
        return 502;
    }
    if let Some(number) = value.as_i64() {
        return number;
    }
    if let Some(text) = value.as_str() {
        return text.parse::<i64>().unwrap_or(502);
    }
    502
}

fn code_value(value: &Value) -> Option<i64> {
    if value.is_boolean() {
        return None;
    }
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value.as_str().and_then(|text| text.parse::<i64>().ok())
}

fn error_parts(error: &Value) -> (Option<i64>, String) {
    if let Some(text) = error.as_str() {
        return (None, text.to_string());
    }
    let code = error
        .get("code")
        .or_else(|| error.get("status"))
        .or_else(|| error.get("statusCode"))
        .and_then(code_value);
    let message = error
        .get("message")
        .or_else(|| error.get("msg"))
        .or_else(|| error.get("error_msg"))
        .and_then(Value::as_str)
        .unwrap_or("Qoder upstream error")
        .to_string();
    (code, message)
}

/// Queue signal: `isQueued`/`queued`, a `RESOURCE_LIMIT*` sub-code, or a nested
/// JSON string carrying either. Returns the suggested wait in seconds.
pub fn queue_hint(body: &str) -> Option<u64> {
    if body.trim().is_empty() || body.len() > MAX_QUEUE_BODY_BYTES {
        return None;
    }
    let payload: Value = serde_json::from_str(body).ok()?;
    queue_in(&payload, 0)
}

fn queue_in(value: &Value, depth: u8) -> Option<u64> {
    if depth > QUEUE_MAX_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => {
            let flagged = QUEUE_FLAG_KEYS
                .iter()
                .any(|key| map.get(*key).is_some_and(truthy));
            let sub_code = map
                .get("subCode")
                .and_then(Value::as_str)
                .map(|text| text.to_ascii_lowercase())
                .is_some_and(|text| contains_any(&text, QUEUE_SUB_CODES));
            if flagged || sub_code {
                return Some(queue_wait(map));
            }
            map.values().find_map(|child| queue_in(child, depth + 1))
        }
        Value::Array(items) => items.iter().find_map(|child| queue_in(child, depth + 1)),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|child| queue_in(&child, depth + 1)),
        _ => None,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_i64().is_some_and(|value| value != 0),
        Value::String(text) => text.eq_ignore_ascii_case("true") || text == "1",
        _ => false,
    }
}

fn queue_wait(map: &serde_json::Map<String, Value>) -> u64 {
    for key in QUEUE_WAIT_KEYS {
        let seconds = map.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_f64().filter(|v| *v > 0.0).map(|v| v as u64))
                .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        });
        if let Some(seconds) = seconds.filter(|seconds| *seconds > 0) {
            return seconds.min(QUEUE_MAX_WAIT_SECONDS);
        }
    }
    QUEUE_DEFAULT_WAIT_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAILY_QUOTA: &str = include_str!("fixtures/error_daily_quota.json");
    const QUEUE: &str = include_str!("fixtures/error_queue_nested.json");
    const SESSION_DEAD: &str = include_str!("fixtures/error_session_dead.json");

    #[test]
    fn auth_statuses_are_distinct() {
        assert_eq!(
            classify_http_status(401, "unauthorized").kind,
            ErrorKind::AuthInvalid
        );
        assert_eq!(
            classify_http_status(403, SESSION_DEAD).kind,
            ErrorKind::AuthInvalid
        );
        assert_eq!(
            classify_http_status(403, "{\"message\":\"TOKEN_EXPIRE\"}").kind,
            ErrorKind::AuthExpired
        );
        assert_eq!(
            classify_http_status(403, "permission denied for this plan").kind,
            ErrorKind::ChannelRestricted
        );
        assert_eq!(
            classify_http_status(403, "risk control").kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn quota_and_daily_limit_are_not_dead_credentials() {
        assert_eq!(
            classify_http_status(402, "balance").kind,
            ErrorKind::QuotaExhausted
        );
        assert_eq!(
            classify_http_status(403, DAILY_QUOTA).kind,
            ErrorKind::QuotaExhausted
        );
    }

    #[test]
    fn nested_queue_string_is_detected() {
        let error = classify_http_status(403, QUEUE);
        assert_eq!(error.kind, ErrorKind::RateLimited);
        assert_eq!(error.retry_after, Some(30));
    }

    #[test]
    fn rate_limits_timeouts_and_server_errors() {
        assert_eq!(classify_http_status(429, "").kind, ErrorKind::RateLimited);
        assert_eq!(classify_http_status(504, "").kind, ErrorKind::Timeout);
        assert_eq!(
            classify_http_status(503, "busy").kind,
            ErrorKind::UpstreamServer
        );
        assert_eq!(
            classify_http_status(418, "teapot").kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn invalid_request_model_and_node_failure() {
        assert_eq!(
            classify_http_status(400, "invalid argument").kind,
            ErrorKind::BadRequest
        );
        assert_eq!(
            classify_http_status(404, "model not found").kind,
            ErrorKind::ModelUnavailable
        );
        assert_eq!(
            classify_http_status(400, "[FAIL]node:oa_qwen msg:Execution failed").kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn device_poll_pending_is_not_an_error() {
        assert!(matches!(
            classify_device_poll(202, ""),
            DevicePollOutcome::Pending
        ));
        assert!(matches!(
            classify_device_poll(404, ""),
            DevicePollOutcome::Pending
        ));
        assert!(matches!(
            classify_device_poll(200, "{}"),
            DevicePollOutcome::Ready
        ));
        assert!(matches!(
            classify_device_poll(500, "err"),
            DevicePollOutcome::Failed(_)
        ));
    }

    #[test]
    fn embedded_payload_errors_are_classified() {
        let payload: Value =
            serde_json::from_str(r#"{"error":{"code":401,"message":"expired"}}"#).unwrap();
        assert_eq!(
            classify_payload(&payload).unwrap().kind,
            ErrorKind::AuthInvalid
        );
        let payload: Value = serde_json::from_str(r#"{"code":"110","message":"x"}"#).unwrap();
        assert_eq!(
            classify_payload(&payload).unwrap().kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn envelope_errors_use_the_same_model_level_rules_as_http() {
        // Model-level transient: the node that serves this model failed.
        assert_eq!(
            classify_envelope(
                400,
                "[FAIL]node:oa_qwen-plus-main msg:Execution failed: null"
            )
            .kind,
            ErrorKind::UpstreamServer
        );
        // The model itself is gone/unavailable; switching accounts does not help.
        assert_eq!(
            classify_envelope(404, r#"{"message":"model not found"}"#).kind,
            ErrorKind::ModelUnavailable
        );
        // Model-level rate limiting.
        assert_eq!(classify_envelope(429, "busy").kind, ErrorKind::RateLimited);
        // Account quota, not a dead credential.
        assert_eq!(
            classify_envelope(402, "balance").kind,
            ErrorKind::QuotaExhausted
        );
        // Qoder uses HTTP-status semantics inside the envelope; the CodeBuddy
        // business codes are not reused here.
        let payload: Value = serde_json::from_str(r#"{"code":"11102","message":"x"}"#).unwrap();
        assert_eq!(
            classify_payload(&payload).unwrap().kind,
            ErrorKind::UpstreamServer
        );
    }

    #[test]
    fn body_secrets_are_redacted_and_truncated() {
        let body = format!(
            "{{\"x-auth-token\":\"super-secret-value\",\"pad\":\"{}\"}}",
            "a".repeat(2000)
        );
        let error = classify_http_status(400, &body);
        assert!(!error.message.contains("super-secret-value"));
        assert!(error.message.chars().count() < 600);
    }
}
