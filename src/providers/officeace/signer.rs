//! Huawei request signing for OfficeAce.
//!
//! Two signature tracks that are not interchangeable:
//!
//! | plane | endpoints | algorithm |
//! | --- | --- | --- |
//! | model gateway | `/v1/subscription*` | `V11-HMAC-SHA256` |
//! | control plane | `/v1/claw/*` | `SDK-HMAC-SHA256` (V4) |
//!
//! V11 differs from V4 in three places (evidence: Coding2API
//! `src/provider/officeace/signer.py`, measured against real gateways):
//!
//! 1. `StringToSign` carries a `YYYYMMDD/{region}/apic` scope line;
//! 2. the signing key is derived — `HMAC(HMAC(AK, SK), scope + 0x01)[:32]` as
//!    **hex text** — where V4 signs with `SK` itself;
//! 3. `X-Sdk-Content-Sha256` is **not** sent (adding it changes the signed
//!    header set and every request answers 401).
//!
//! The canonical primitives are shared with [`crate::providers::codearts::signer`]
//! so both Huawei tracks encode paths and queries identically.

use std::fmt;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

use crate::providers::codearts::signer::{
    canonical_headers, canonical_query, canonical_uri, sdk_date, sha256_hex, HEADER_X_SDK_DATE,
    HEADER_X_SECURITY_TOKEN,
};

type HmacSha256 = Hmac<Sha256>;

pub const ALGORITHM_V11: &str = "V11-HMAC-SHA256";
pub const ALGORITHM_V4: &str = "SDK-HMAC-SHA256";
/// APIG always uses `apic` as the credential scope service.
pub const SCOPE_SERVICE: &str = "apic";

#[derive(Clone)]
pub struct SignCredential {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub security_token: String,
    pub project_id: String,
}

impl fmt::Debug for SignCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignCredential")
            .field("access_key_id", &"<redacted>")
            .field("security_token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn hmac_bytes(key: &[u8], message: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().into()
}

fn hmac_hex(key: &[u8], message: &str) -> String {
    hex::encode(hmac_bytes(key, message))
}

/// V11 signing-key derivation. Every detail here was measured:
/// outer key is the **AK**, message is the **SK**; the intermediate digest is
/// truncated to 32 bytes and **hex-encoded** before the final HMAC; the
/// `scope + 0x01` separator is mandatory.
pub fn signing_key(secret_access_key: &str, scope: &str, access_key_id: &str) -> String {
    let step1 = hmac_bytes(access_key_id.as_bytes(), secret_access_key);
    hex::encode(&hmac_bytes(&step1, &format!("{scope}\x01"))[..32])
}

fn merged_headers(
    credential: &SignCredential,
    url: &Url,
    headers: &[(String, String)],
    now: DateTime<Utc>,
    with_project: bool,
) -> Vec<(String, String)> {
    let mut merged = headers.to_vec();
    let has = |name: &str, set: &[(String, String)]| {
        set.iter().any(|(key, _)| key.eq_ignore_ascii_case(name))
    };
    if !has(HEADER_X_SDK_DATE, &merged) {
        merged.push((HEADER_X_SDK_DATE.to_string(), sdk_date(now)));
    }
    if !has("host", &merged) {
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        merged.push(("host".into(), host));
    }
    if !credential.security_token.is_empty() && !has(HEADER_X_SECURITY_TOKEN, &merged) {
        merged.push((
            HEADER_X_SECURITY_TOKEN.into(),
            credential.security_token.clone(),
        ));
    }
    if with_project && !credential.project_id.is_empty() && !has("X-Project-ID", &merged) {
        merged.push(("X-Project-ID".into(), credential.project_id.clone()));
    }
    merged
}

/// Canonical request shared by both tracks. The query string participates in
/// the signature; leaving it out is the classic `APIG.0301` failure.
fn canonical_request(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    payload_hash: &str,
) -> (String, String) {
    let (header_text, signed) = canonical_headers(headers);
    let request = [
        method.to_ascii_uppercase(),
        canonical_uri(url.path()),
        canonical_query(url.query()),
        header_text,
        signed.clone(),
        payload_hash.to_string(),
    ]
    .join("\n");
    (request, signed)
}

/// Complete header set for a V11-signed gateway request.
pub fn v11_headers(
    credential: &SignCredential,
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    now: DateTime<Utc>,
    region: &str,
) -> Vec<(String, String)> {
    let mut merged = merged_headers(credential, url, headers, now, true);
    let date = merged
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(HEADER_X_SDK_DATE))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let (request, signed) = canonical_request(method, url, &merged, &sha256_hex(body));
    let scope = format!("{}/{region}/{SCOPE_SERVICE}", &date[..8.min(date.len())]);
    let string_to_sign = [
        ALGORITHM_V11.to_string(),
        date,
        scope.clone(),
        sha256_hex(request.as_bytes()),
    ]
    .join("\n");
    let key = signing_key(
        &credential.secret_access_key,
        &scope,
        &credential.access_key_id,
    );
    let signature = hmac_hex(key.as_bytes(), &string_to_sign);
    merged.push((
        "Authorization".into(),
        format!(
            "{ALGORITHM_V11} Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
            credential.access_key_id
        ),
    ));
    merged
}

/// Complete header set for a V4-signed control-plane request. No
/// `X-Sdk-Content-Sha256`: the reference track does not send it and Huawei
/// signs exactly the headers that are present.
pub fn v4_headers(
    credential: &SignCredential,
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    now: DateTime<Utc>,
) -> Vec<(String, String)> {
    let mut merged = merged_headers(credential, url, headers, now, true);
    let date = merged
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(HEADER_X_SDK_DATE))
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    let (request, signed) = canonical_request(method, url, &merged, &sha256_hex(body));
    let string_to_sign = [
        ALGORITHM_V4.to_string(),
        date,
        sha256_hex(request.as_bytes()),
    ]
    .join("\n");
    let signature = hmac_hex(credential.secret_access_key.as_bytes(), &string_to_sign);
    merged.push((
        "Authorization".into(),
        format!(
            "{ALGORITHM_V4} Access={}, SignedHeaders={signed}, Signature={signature}",
            credential.access_key_id
        ),
    ));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-30T03:04:05Z")
            .expect("date")
            .with_timezone(&Utc)
    }

    fn credential() -> SignCredential {
        SignCredential {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "secret".into(),
            security_token: "sts-token".into(),
            project_id: "project-1".into(),
        }
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .unwrap_or_default()
    }

    #[test]
    fn v11_signing_key_uses_ak_as_key_and_sk_as_message() {
        let key = signing_key("sk-value", "20261030/cn-southwest-2/apic", "ak-value");
        assert_eq!(key.len(), 64);
        // Independent recomputation of the documented derivation.
        let step1 = hmac_bytes(b"ak-value", "sk-value");
        let step2 = hmac_bytes(&step1, "20261030/cn-southwest-2/apic\x01");
        assert_eq!(key, hex::encode(&step2[..32]));
    }

    /// Byte-for-byte cross-check against the reference implementation
    /// (`provider/officeace/signer.py`, run in isolation on the same inputs).
    #[test]
    fn vectors_match_the_reference_implementation() {
        assert_eq!(
            signing_key("secret", "20261030/cn-southwest-2/apic", "AKIDEXAMPLE"),
            "d63bb620594bd7f7eb1b08da4094c8f2199e47aadbed9790aacbf0407f798a77"
        );
        let gateway = Url::parse("https://gw.example/v1/subscription").expect("url");
        let signed = v11_headers(
            &credential(),
            "GET",
            &gateway,
            &[("x-subscription-type".into(), "v2".into())],
            b"",
            date(),
            "cn-southwest-2",
        );
        assert_eq!(
            header(&signed, "authorization"),
            "V11-HMAC-SHA256 Credential=AKIDEXAMPLE/20261030/cn-southwest-2/apic, \
             SignedHeaders=host;x-project-id;x-sdk-date;x-security-token;x-subscription-type, \
             Signature=be740a79616579e76a95ddfcbb8c2ba88b4d951bd47b13f8ec0e2bf9138169df"
        );
        let control = Url::parse("https://agentarts.example/v1/claw/auth/state").expect("url");
        let signed = v4_headers(&credential(), "POST", &control, &[], b"{}", date());
        assert_eq!(
            header(&signed, "authorization"),
            "SDK-HMAC-SHA256 Access=AKIDEXAMPLE, \
             SignedHeaders=host;x-project-id;x-sdk-date;x-security-token, \
             Signature=5f7290bbb3c63292882f5ccb01b9b78dfcdfc36231ed74171cbe1676d0787af4"
        );
    }

    #[test]
    fn v11_authorization_carries_scope_and_signed_headers() {
        let url = Url::parse("https://gw.example/v1/subscription?x=1").expect("url");
        let headers = v11_headers(
            &credential(),
            "GET",
            &url,
            &[("x-subscription-type".into(), "v2".into())],
            b"",
            date(),
            "cn-southwest-2",
        );
        let auth = header(&headers, "authorization");
        assert!(auth.starts_with(
            "V11-HMAC-SHA256 Credential=AKIDEXAMPLE/20261030/cn-southwest-2/apic, SignedHeaders="
        ));
        assert!(auth.contains("x-subscription-type"));
        assert!(auth.contains("x-project-id"));
        assert!(!headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("X-Sdk-Content-Sha256")));
        let debug = format!("{:?}", credential());
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("sts-token"));
    }

    #[test]
    fn the_query_string_changes_the_signature() {
        let plain = Url::parse("https://gw.example/v1/subscription").expect("url");
        let query = Url::parse("https://gw.example/v1/subscription?sku=1").expect("url");
        let first = v11_headers(
            &credential(),
            "GET",
            &plain,
            &[],
            b"",
            date(),
            "cn-southwest-2",
        );
        let second = v11_headers(
            &credential(),
            "GET",
            &query,
            &[],
            b"",
            date(),
            "cn-southwest-2",
        );
        assert_ne!(
            header(&first, "authorization"),
            header(&second, "authorization")
        );
    }

    #[test]
    fn v4_has_no_scope_line_and_no_content_hash_header() {
        let url = Url::parse("https://agentarts.example/v1/claw/auth/state").expect("url");
        let headers = v4_headers(&credential(), "POST", &url, &[], b"{}", date());
        let auth = header(&headers, "authorization");
        assert!(auth.starts_with("SDK-HMAC-SHA256 Access=AKIDEXAMPLE, SignedHeaders="));
        assert!(!headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("X-Sdk-Content-Sha256")));
    }

    #[test]
    fn signing_is_deterministic_for_a_fixed_clock() {
        let url = Url::parse("https://gw.example/v1/subscription").expect("url");
        let first = v11_headers(
            &credential(),
            "GET",
            &url,
            &[],
            b"",
            date(),
            "cn-southwest-2",
        );
        let second = v11_headers(
            &credential(),
            "GET",
            &url,
            &[],
            b"",
            date(),
            "cn-southwest-2",
        );
        assert_eq!(first, second);
    }
}
