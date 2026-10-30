//! Huawei cloud `SDK-HMAC-SHA256` request signing.
//!
//! Ported from the reference implementations (local `app/codearts/signer.py`
//! and Coding2API `src/provider/codearts/signer.py`, both read-only sources):
//!
//! 1. `CanonicalURI` percent-encodes every path segment and always ends in `/`
//!    (`/v1/model/builtin` -> `/v1/model/builtin/`). A missing trailing slash is
//!    a common `APIG.0301` signature failure.
//! 2. `SignedHeaders` is every header that will be sent (lowercase, sorted).
//!    `Authorization` is added after signing and is not part of the set.
//! 3. The payload hash is the value of `X-Sdk-Content-Sha256`, which is written
//!    before the canonical request is built.
//! 4. `StringToSign` = algorithm, `X-Sdk-Date`, hex(sha256(canonical request)).
//!
//! Secrets never appear in `Debug` output: [`SignCredential`] redacts them.

use std::fmt;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use url::Url;

type HmacSha256 = Hmac<Sha256>;

pub const SIGNING_ALGORITHM: &str = "SDK-HMAC-SHA256";
pub const HEADER_X_SDK_DATE: &str = "X-Sdk-Date";
pub const HEADER_X_SECURITY_TOKEN: &str = "X-Security-Token";
pub const HEADER_X_SDK_CONTENT_SHA256: &str = "X-Sdk-Content-Sha256";

#[derive(Clone)]
pub struct SignCredential {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub security_token: String,
}

impl fmt::Debug for SignCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignCredential")
            .field("access_key_id", &"<redacted>")
            .field("security_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct SignedRequest {
    /// Complete header set to send, including `Authorization`.
    pub headers: Vec<(String, String)>,
}

pub fn sha256_hex(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

pub fn sdk_date(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

fn hmac_sha256_hex(secret: &str, message: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Percent-encode `value`, keeping the characters in `safe` verbatim.
fn percent_encode(value: &str, safe: &[u8]) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(*byte, b'-' | b'_' | b'.' | b'~')
            || safe.contains(byte);
        if keep {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Huawei `encodeURIComponent` equivalent for path segments.
fn encode_uri_component(segment: &str) -> String {
    percent_encode(segment, b"!*'()")
}

pub fn canonical_uri(path: &str) -> String {
    let encoded = path
        .split('/')
        .map(encode_uri_component)
        .collect::<Vec<_>>()
        .join("/");
    if encoded.ends_with('/') {
        encoded
    } else {
        format!("{encoded}/")
    }
}

pub(crate) fn canonical_query(query: Option<&str>) -> String {
    let Some(raw) = query else {
        return String::new();
    };
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(key, value)| (percent_encode(&key, b""), percent_encode(&value, b"")))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn clean_header_value(value: &str) -> String {
    value.replace(['\r', '\n'], "").trim().to_string()
}

pub(crate) fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut lowered: Vec<(String, String)> = headers
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), clean_header_value(value)))
        .collect();
    lowered.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = lowered
        .iter()
        .map(|(key, value)| format!("{key}:{value}\n"))
        .collect::<String>();
    let signed = lowered
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, signed)
}

/// Sign one request.
///
/// `headers` must contain every header that will be sent. `Host` is derived
/// from `url` when absent; `X-Sdk-Date`, `X-Security-Token` and
/// `X-Sdk-Content-Sha256` are added here.
pub fn sign_request(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    credential: &SignCredential,
    now: DateTime<Utc>,
) -> SignedRequest {
    let mut merged: Vec<(String, String)> = headers.to_vec();
    let has = |name: &str, set: &[(String, String)]| {
        set.iter().any(|(key, _)| key.eq_ignore_ascii_case(name))
    };

    if !has(HEADER_X_SDK_DATE, &merged) {
        merged.push((HEADER_X_SDK_DATE.into(), sdk_date(now)));
    }
    if !has("host", &merged) {
        merged.push(("host".into(), url_host(url)));
    }
    if !credential.security_token.is_empty() && !has(HEADER_X_SECURITY_TOKEN, &merged) {
        merged.push((
            HEADER_X_SECURITY_TOKEN.into(),
            credential.security_token.clone(),
        ));
    }
    let payload_hash = sha256_hex(body);
    if !has(HEADER_X_SDK_CONTENT_SHA256, &merged) {
        merged.push((HEADER_X_SDK_CONTENT_SHA256.into(), payload_hash.clone()));
    }

    let sdk_date = merged
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(HEADER_X_SDK_DATE))
        .map(|(_, value)| clean_header_value(value))
        .unwrap_or_default();
    let (canonical_header_text, signed_headers) = canonical_headers(&merged);
    let canonical_request = [
        method.to_ascii_uppercase(),
        canonical_uri(url.path()),
        canonical_query(url.query()),
        canonical_header_text,
        signed_headers.clone(),
        payload_hash,
    ]
    .join("\n");

    let string_to_sign = [
        SIGNING_ALGORITHM.to_string(),
        sdk_date,
        sha256_hex(canonical_request.as_bytes()),
    ]
    .join("\n");
    let signature = hmac_sha256_hex(&credential.secret_access_key, &string_to_sign);

    merged.push((
        "Authorization".into(),
        format!(
            "{SIGNING_ALGORITHM} Access={}, SignedHeaders={signed_headers}, Signature={signature}",
            credential.access_key_id
        ),
    ));

    SignedRequest { headers: merged }
}

fn url_host(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T12:34:56Z")
            .expect("date")
            .with_timezone(&Utc)
    }

    fn credential() -> SignCredential {
        SignCredential {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "secret".into(),
            security_token: "token-value".into(),
        }
    }

    #[test]
    fn canonical_uri_encodes_segments_and_adds_trailing_slash() {
        assert_eq!(canonical_uri("/v1/model/builtin"), "/v1/model/builtin/");
        assert_eq!(canonical_uri("/a b/c"), "/a%20b/c/");
        assert_eq!(canonical_uri("/"), "/");
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        assert_eq!(canonical_query(Some("b=2&a=1")), "a=1&b=2");
        assert_eq!(canonical_query(Some("channel=IDE")), "channel=IDE");
        assert_eq!(canonical_query(None), "");
    }

    #[test]
    fn signature_is_deterministic_and_redacts_secrets_in_debug() {
        let url = Url::parse("https://snap-access.cn-north-4.myhuaweicloud.com/v1/model/builtin")
            .expect("url");
        let first = sign_request(
            "GET",
            &url,
            &[("Content-Type".into(), "application/json".into())],
            b"",
            &credential(),
            date(),
        );
        let second = sign_request(
            "GET",
            &url,
            &[("Content-Type".into(), "application/json".into())],
            b"",
            &credential(),
            date(),
        );
        assert_eq!(first.headers, second.headers);
        let auth = first
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone())
            .expect("authorization");
        assert!(auth.starts_with("SDK-HMAC-SHA256 Access=AKIDEXAMPLE, SignedHeaders="));
        assert!(auth.contains("Signature="));
        let debug = format!("{:?}", credential());
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("token-value"));
    }

    #[test]
    fn header_values_never_break_the_canonical_request() {
        let url = Url::parse("https://example.com/x").expect("url");
        let signed = sign_request(
            "POST",
            &url,
            &[("X-Test".into(), "a\r\nb".into())],
            b"{}",
            &credential(),
            date(),
        );
        let auth = signed
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .expect("authorization")
            .1
            .clone();
        assert!(!auth.contains('\n'));
        assert!(auth.contains("x-test"));
    }
}
