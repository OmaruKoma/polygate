//! `X-SA-*` request signing.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md` §5, recovered from the desktop Go
//! engine (`(*GatewaySigner).Sign` @ `0x140d9a2e0`):
//!
//! ```text
//! k1  = HMAC-SHA256(key = jwt3, msg = "superagent-auth-v1"/token/installId/ts/nonce)
//! sig = HMAC-SHA256(key = k1hex, msg = "superagent-auth-v1"\nMETHOD\nuri\nts\nnonce\nappVersion\nsha256hex(body))
//! ```
//!
//! Both HMAC keys are ASCII hex strings, not the raw digests: the verified
//! reference script in §6 of the migration note does exactly the same and was
//! accepted by the live upstream. The body hash must cover the **exact bytes**
//! that are sent — re-serialising the JSON changes them and breaks the
//! signature (measured: two MISMATCH replays from `llm_tap` output).

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use super::region;

type HmacSha256 = Hmac<Sha256>;

/// JWT third segment used as the outer HMAC key.
pub fn jwt_third_segment(token: &str) -> &str {
    token.rsplit('.').next().unwrap_or(token)
}

pub fn sha256_hex(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    hex::encode(digest)
}

fn hmac_hex(key: &str, message: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// First HMAC layer: binds the token to one device identity and nonce.
pub fn step_key(token: &str, install_id: &str, timestamp: &str, nonce: &str) -> String {
    hmac_hex(
        jwt_third_segment(token),
        &[region::SIGN_MAGIC, token, install_id, timestamp, nonce].join("/"),
    )
}

/// Final signature over method, URI, time, nonce, app version and body hash.
pub fn sign(
    step: &str,
    method: &str,
    uri: &str,
    timestamp: &str,
    nonce: &str,
    app_version: &str,
    body: &[u8],
) -> String {
    hmac_hex(
        step,
        &[
            region::SIGN_MAGIC,
            method,
            uri,
            timestamp,
            nonce,
            app_version,
            &sha256_hex(body),
        ]
        .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vector computed with an independent Node.js implementation of
    /// the documented algorithm; matching it proves no accidental sign drift.
    #[test]
    fn the_documented_algorithm_matches_the_golden_vector() {
        let token = "Em9xR4:aaa.bbb.ccc";
        let install = "install-123";
        let timestamp = "1700000000";
        let nonce = "nonce-1";
        let body = br#"{"model":"chat-pro"}"#;

        let step = step_key(token, install, timestamp, nonce);
        assert_eq!(
            step,
            "3e4177afadff64ea32c64ffafbe403e167220915a0b4677ec1795c8abf7ebd39"
        );
        let signature = sign(
            &step,
            "POST",
            "/superCowork/sapi/api/v1/chat/completions",
            timestamp,
            nonce,
            "3.1.0",
            body,
        );
        assert_eq!(
            signature,
            "05b80e74f58afad1e438a4102408a2fc6ebb33c05a02cc094cbffc36305b07cd"
        );
        assert_eq!(
            sha256_hex(body),
            "51c7c91721a45b24be9f1e000006da7d3be4294b24434207b2cce64a47d74ff0"
        );
    }

    #[test]
    fn the_key_is_the_last_jwt_segment() {
        assert_eq!(jwt_third_segment("t:a.b.c"), "c");
        assert_eq!(jwt_third_segment("plain"), "plain");
    }
}
