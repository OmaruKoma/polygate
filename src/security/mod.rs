//! Credential encryption and token hashing.
//!
//! * Upstream credentials are encrypted with AES-256-GCM before they touch disk.
//! * Gateway API keys / admin tokens are stored as SHA-256 hashes (high entropy
//!   random tokens, so a KDF is not required; argon2 is used for optional
//!   passwords).
//! * The master key comes from `POLYGATE_MASTER_KEY` or a 0600 file in the data
//!   directory generated on first boot.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{AeadCore, Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::error::GatewayError;

const KEY_LEN: usize = 32;
const ENC_PREFIX: &str = "v1:";
/// Ciphertext version persisted alongside encrypted credentials.
pub const ENCRYPTION_VERSION: &str = "v1";
/// Domain separator so a credential ciphertext can never be opened as some
/// other kind of encrypted blob.
const AAD_DOMAIN: &str = "polygate:credential:v1:";

/// Holds the symmetric key used for credential encryption.
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    pub fn from_env_or_file(data_dir: &Path) -> Result<Self, GatewayError> {
        let mut key = match std::env::var("POLYGATE_MASTER_KEY") {
            Ok(v) => decode_key(&v).ok_or_else(|| {
                GatewayError::config("POLYGATE_MASTER_KEY must be 32 bytes, hex or base64")
            })?,
            Err(_) => load_or_create_key_file(&data_dir.join("master.key"))?,
        };
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| GatewayError::config("invalid encryption key"))?;
        key.zeroize();
        Ok(Self { cipher })
    }

    /// Encrypt without AAD. Use [`SecretBox::encrypt_for_account`] for
    /// credentials so the ciphertext stays bound to one account.
    /// Build a box from explicit key material (tests / `doctor`). Production
    /// paths must use [`SecretBox::from_env_or_file`].
    pub fn from_master_key(key: &[u8]) -> Result<Self, GatewayError> {
        if key.len() != KEY_LEN {
            return Err(GatewayError::config("master key must be 32 bytes"));
        }
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| GatewayError::config("invalid encryption key"))?;
        Ok(Self { cipher })
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<String, GatewayError> {
        self.encrypt_with_aad(&[], plaintext)
    }

    pub fn encrypt_with_aad(&self, aad: &[u8], plaintext: &[u8]) -> Result<String, GatewayError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| GatewayError::internal("credential encryption failed"))?;
        let mut blob = Vec::with_capacity(nonce.len() + ciphertext.len());
        blob.extend_from_slice(nonce.as_slice());
        blob.extend_from_slice(&ciphertext);
        Ok(format!(
            "{ENC_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(blob)
        ))
    }

    /// Encrypt a credential for one account. Decrypting it under a different
    /// account id fails the AEAD check.
    pub fn encrypt_for_account(
        &self,
        account_id: &str,
        plaintext: &[u8],
    ) -> Result<String, GatewayError> {
        self.encrypt_with_aad(&account_aad(account_id), plaintext)
    }

    pub fn decrypt(&self, encoded: &str) -> Result<Vec<u8>, GatewayError> {
        self.decrypt_with_aad(&[], encoded)
    }

    pub fn decrypt_with_aad(&self, aad: &[u8], encoded: &str) -> Result<Vec<u8>, GatewayError> {
        let b64 = encoded
            .strip_prefix(ENC_PREFIX)
            .ok_or_else(|| GatewayError::internal("unsupported credential ciphertext version"))?;
        let blob = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| GatewayError::internal("corrupt credential ciphertext"))?;
        if blob.len() < 12 {
            return Err(GatewayError::internal("corrupt credential ciphertext"));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let mut plain = self
            .cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| {
                GatewayError::internal(
                    "credential decryption failed (wrong master key or account binding?)",
                )
            })?;
        // Callers parse immediately; zeroization is best-effort on this buffer.
        let out = plain.clone();
        plain.zeroize();
        Ok(out)
    }

    pub fn decrypt_for_account(
        &self,
        account_id: &str,
        encoded: &str,
    ) -> Result<Vec<u8>, GatewayError> {
        self.decrypt_with_aad(&account_aad(account_id), encoded)
    }
}

/// AAD used to bind a credential ciphertext to its account id.
pub fn account_aad(account_id: &str) -> Vec<u8> {
    format!("{AAD_DOMAIN}{account_id}").into_bytes()
}

fn decode_key(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim();
    if let Ok(bytes) = hex::decode(trimmed) {
        if bytes.len() == KEY_LEN {
            return Some(bytes);
        }
    }
    for engine in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(trimmed) {
            if bytes.len() == KEY_LEN {
                return Some(bytes);
            }
        }
    }
    None
}

fn load_or_create_key_file(path: &Path) -> Result<Vec<u8>, GatewayError> {
    if let Ok(raw) = std::fs::read_to_string(path) {
        let trimmed = raw.trim();
        if let Some(key) = decode_key(trimmed) {
            warn_if_permissive(path);
            return Ok(key);
        }
        return Err(GatewayError::config(format!(
            "invalid master key file {}",
            path.display()
        )));
    }
    let mut key = vec![0u8; KEY_LEN];
    rand::thread_rng().fill_bytes(&mut key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| GatewayError::config(format!("cannot create data dir: {e}")))?;
    }
    std::fs::write(path, hex::encode(&key))
        .map_err(|e| GatewayError::config(format!("cannot write master key: {e}")))?;
    restrict_permissions(path)?;
    tracing::warn!(
        path = %path.display(),
        "generated new credential encryption master key; back it up, losing it makes credentials unreadable"
    );
    Ok(key)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), GatewayError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| GatewayError::config(format!("cannot chmod master key: {e}")))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), GatewayError> {
    Ok(())
}

#[cfg(unix)]
fn warn_if_permissive(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                mode = format!("{mode:o}"),
                "master key file is readable by group/others; expected 0600"
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_if_permissive(_path: &Path) {}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

/// Generate a high-entropy token with a human-recognisable prefix.
pub fn generate_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    format!("{prefix}{encoded}")
}

/// SHA-256 hash used for storing gateway keys and admin tokens.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn verify_token(token: &str, expected_hash: &str) -> bool {
    let actual = hash_token(token);
    actual.as_bytes().ct_eq(expected_hash.as_bytes()).into()
}

/// Mask a credential value for logs and the admin UI.
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "••••••".into();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// Produce a hint from a credential map without exposing secrets.
/// Keys are shown, values are masked.
pub fn credential_hint(credential: &serde_json::Value) -> String {
    match credential {
        serde_json::Value::Object(map) => {
            let keys: Vec<&String> = map.keys().take(6).collect();
            keys.iter()
                .map(|k| format!("{k}=***"))
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => "credential".into(),
    }
}

/// Path helper exposed for tests.
pub fn default_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("master.key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn test_box(key: [u8; KEY_LEN]) -> SecretBox {
        SecretBox {
            cipher: Aes256Gcm::new_from_slice(&key).unwrap(),
        }
    }

    #[test]
    fn encrypt_roundtrip_and_tamper_detection() {
        let sb = test_box([7u8; KEY_LEN]);
        let enc = sb.encrypt(b"{\"token\":\"secret\"}").unwrap();
        assert!(enc.starts_with("v1:"));
        assert_eq!(sb.decrypt(&enc).unwrap(), b"{\"token\":\"secret\"}");
        let mut tampered = enc.clone().into_bytes();
        let idx = tampered.len() - 2;
        tampered[idx] ^= 1;
        assert!(sb.decrypt(std::str::from_utf8(&tampered).unwrap()).is_err());
    }

    #[test]
    fn aad_roundtrip_binds_ciphertext_to_account() {
        let sb = test_box([9u8; KEY_LEN]);
        let enc = sb
            .encrypt_for_account("acct-a", b"{\"token\":\"t\"}")
            .unwrap();
        assert_eq!(
            sb.decrypt_for_account("acct-a", &enc).unwrap(),
            b"{\"token\":\"t\"}"
        );

        let wrong_account = sb.decrypt_for_account("acct-b", &enc).unwrap_err();
        assert_eq!(wrong_account.kind, ErrorKind::Internal);
        // An empty AAD must not open an account-bound ciphertext either.
        assert!(sb.decrypt(&enc).is_err());

        assert_eq!(account_aad("acct-a"), account_aad("acct-a"));
        assert_ne!(account_aad("acct-a"), account_aad("acct-b"));
        assert!(account_aad("acct-a").starts_with(b"polygate:credential:v1:"));
    }

    #[test]
    fn wrong_master_key_and_tampered_aad_blob_are_rejected() {
        let writer = test_box([1u8; KEY_LEN]);
        let reader = test_box([2u8; KEY_LEN]);
        let enc = writer.encrypt_for_account("acct", b"secret").unwrap();
        let err = reader.decrypt_for_account("acct", &enc).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Internal);

        let mut tampered = enc.into_bytes();
        let idx = tampered.len() - 2;
        tampered[idx] ^= 1;
        assert!(writer
            .decrypt_for_account("acct", std::str::from_utf8(&tampered).unwrap())
            .is_err());
    }

    #[test]
    fn master_key_file_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("master.key");
        let first = load_or_create_key_file(&path).unwrap();
        assert_eq!(first.len(), KEY_LEN);
        assert!(path.exists());
        let reloaded = load_or_create_key_file(&path).unwrap();
        assert_eq!(first, reloaded);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn invalid_master_key_file_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        std::fs::write(&path, "too-short").unwrap();
        let err = load_or_create_key_file(&path).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(!err.message.contains("too-short"));
    }

    #[test]
    fn token_hash_is_deterministic_and_verifiable() {
        let token = generate_token("pg-");
        let hash = hash_token(&token);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash, token);
        assert!(verify_token(&token, &hash));
        assert!(!verify_token("pg-wrong", &hash));
        assert_eq!(hash_token("same-input"), hash_token("same-input"));
        assert_ne!(hash_token("a"), hash_token("b"));
    }

    #[test]
    fn generated_tokens_are_unique_and_prefixed() {
        let a = generate_token("pg-");
        let b = generate_token("pg-");
        assert!(a.starts_with("pg-"));
        assert!(b.starts_with("pg-"));
        assert_ne!(a, b);
    }

    #[test]
    fn log_redaction_covers_authorization_cookie_and_token() {
        let log_line = "request failed\nAuthorization: Bearer sk-live-abc123\nCookie: session=s3cr3t; theme=dark\ntoken=plain-token-9";
        let out = crate::error::redact_secrets(log_line);
        assert!(!out.contains("sk-live-abc123"));
        assert!(!out.contains("s3cr3t"));
        assert!(!out.contains("theme=dark"));
        assert!(!out.contains("plain-token-9"));
        assert!(out.contains("Authorization: ***"));
        assert!(out.contains("Cookie: ***"));
        assert!(out.contains("token=***"));
    }

    #[test]
    fn credential_hint_never_contains_values() {
        let credential = serde_json::json!({"token": "abc", "cookie": "sid=1"});
        let hint = credential_hint(&credential);
        assert!(hint.contains("token=***"));
        assert!(hint.contains("cookie=***"));
        assert!(!hint.contains("abc"));
        assert!(!hint.contains("sid=1"));
    }

    #[test]
    fn mask_never_reveals_middle() {
        assert_eq!(mask_secret("abcdefghijklmnop"), "abcd…mnop");
        assert_eq!(mask_secret("short"), "••••••");
    }
}
