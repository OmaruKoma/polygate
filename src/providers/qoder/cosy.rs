//! Qoder COSY request signing.
//!
//! The scheme below is the one the installed 1.1.71 clients actually use, and it
//! is live-verified against the CN gateway (`2026-10-29`):
//!
//! * the body is encoded with a custom Base64 variant (`qoder_encode`);
//! * `Authorization: Bearer COSY.<payload_b64>.<md5>`;
//! * the signature input is `payload_b64 \n cosy_key \n date \n body \n path`
//!   where `path` is the URL path with the leading `/algo` removed and the query
//!   string excluded;
//! * `info` is AES-128-CBC over a fixed-order identity JSON (key == iv), and
//!   `cosy_key` is the same 16-byte key wrapped with the embedded RSA public
//!   key (PKCS#1 v1.5);
//! * the payload envelope is `{"version","requestId","info","cosyVersion",
//!   "ideVersion"}` in that exact order.
//!
//! Discovery note: the earlier Coding2API shape (commit `df99362…`) sorted the
//! payload keys and sent a richer identity (refresh token, organization, user
//! type, `aid` = uid, `yx_uid`). The CN gateway answers that with `403
//! {"code":"101","message":"Signature invalid"}`. The identity and envelope
//! order here match the official Electron client (`resources/app.asar`,
//! `lXt`/`iqe`) and were confirmed by a live model-list and chat request. Only
//! the four signed headers plus a fixed User-Agent are sent; the client-identity
//! header family (`Cosy-ClientType`, `Cosy-MachineId`, …) is **not** needed.
//!
//! Nothing here performs network I/O. Time and request ids are injected so tests
//! can use fixed vectors. Secrets never appear in `Debug` output.

use std::fmt;
use std::sync::OnceLock;

use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use md5::{Digest, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

use crate::error::GatewayError;
use crate::providers::profile::{self, Identity};

/// Public key used only to wrap the ephemeral AES session key. It is the same
/// constant the official clients embed; it is not a secret.
const SERVER_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

/// Value sent in the payload's `cosyVersion`. The server accepted `1.0.0` and
/// `1.1.71` in live probes; the installed client's version is used.
pub const COSY_VERSION: &str = "1.1.71";
pub const CLIENT_UA: &str = "Go-http-client/2.0";

pub const STANDARD_ALPHABET: &str =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub const CUSTOM_ALPHABET: &str =
    "_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
pub const CUSTOM_PAD: char = '$';

/// Identity fields carried inside the encrypted `info` blob.
///
/// `access_token` is a secret: the custom `Debug` impl redacts it and this type
/// intentionally has no `Serialize` impl.
#[derive(Clone)]
pub struct CosyIdentity {
    pub uid: String,
    pub access_token: String,
    pub name: String,
}

impl fmt::Debug for CosyIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CosyIdentity")
            .field("uid", &self.uid)
            .field("access_token", &"<redacted>")
            .field("name", &self.name)
            .finish()
    }
}

impl CosyIdentity {
    pub fn new(uid: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            access_token: access_token.into(),
            name: String::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

/// One signed request. `authorization` contains the encrypted identity and must
/// not be logged; `Debug` only exposes the path and timestamp.
#[derive(Clone)]
pub struct CosySignedRequest {
    pub payload_b64: String,
    pub date: String,
    pub authorization: String,
    pub path: String,
}

impl fmt::Debug for CosySignedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CosySignedRequest")
            .field("payload_b64", &"<redacted>")
            .field("date", &self.date)
            .field("authorization", &"<redacted>")
            .field("path", &self.path)
            .finish()
    }
}

/// Process-wide COSY signing material for one credential.
///
/// The material depends on the access token, so a token rotation must build a
/// new session. The caller owns that cache.
#[derive(Clone)]
pub struct CosySession {
    uid: String,
    info: String,
    cosy_key: String,
    identity: Identity,
}

impl fmt::Debug for CosySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CosySession")
            .field("uid", &self.uid)
            .field("has_info", &!self.info.is_empty())
            .field("has_cosy_key", &!self.cosy_key.is_empty())
            .finish()
    }
}

impl CosySession {
    /// Build a session from an identity and an explicit 16-byte temporary key.
    pub fn new(identity: CosyIdentity, temp_key: [u8; 16]) -> Result<Self, GatewayError> {
        Self::build(identity, temp_key, server_public_key()?)
    }

    /// Build a session with a fresh temporary key.
    ///
    /// Both official clients use 16 hex characters (a uuid without dashes,
    /// truncated) rather than arbitrary bytes; the server treats the decrypted
    /// key as text, so the same shape is used here.
    pub fn new_random(identity: CosyIdentity) -> Result<Self, GatewayError> {
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let mut temp_key = [0u8; 16];
        temp_key.copy_from_slice(&uuid.as_bytes()[..16]);
        Self::new(identity, temp_key)
    }

    #[cfg(test)]
    pub(crate) fn with_public_key(
        identity: CosyIdentity,
        temp_key: [u8; 16],
        public_key: &RsaPublicKey,
    ) -> Result<Self, GatewayError> {
        Self::build(identity, temp_key, public_key)
    }

    fn build(
        identity: CosyIdentity,
        temp_key: [u8; 16],
        public_key: &RsaPublicKey,
    ) -> Result<Self, GatewayError> {
        if identity.access_token.is_empty() {
            return Err(GatewayError::auth_invalid(
                "Qoder credential is missing its access token",
            ));
        }

        // Field order is part of the protocol: the server decrypts this JSON and
        // both clients emit it in exactly this order.
        let identity_json = format!(
            "{{\"uid\":{},\"aid\":\"\",\"name\":{},\"email\":\"\",\"security_oauth_token\":{}}}",
            json_string(&identity.uid),
            json_string(&identity.name),
            json_string(&identity.access_token),
        );

        let info = BASE64.encode(aes_cbc_encrypt(identity_json.as_bytes(), &temp_key));
        let cosy_key = BASE64.encode(
            public_key
                .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, &temp_key)
                .map_err(|_| GatewayError::internal("cannot protect the Qoder session key"))?,
        );

        Ok(Self {
            uid: identity.uid,
            info,
            cosy_key,
            identity: Identity::default(),
        })
    }

    pub fn uid(&self) -> &str {
        &self.uid
    }

    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = identity;
    }

    /// Encrypted identity blob (base64). Do not log.
    pub fn info(&self) -> &str {
        &self.info
    }

    /// RSA-wrapped temporary key (base64). Do not log.
    pub fn cosy_key(&self) -> &str {
        &self.cosy_key
    }

    /// Sign an already-encoded body. `date` is Unix seconds as a decimal string.
    pub fn sign(
        &self,
        body: &str,
        raw_url: &str,
        date: &str,
        request_id: &str,
    ) -> Result<CosySignedRequest, GatewayError> {
        let payload = format!(
            "{{\"version\":\"v1\",\"requestId\":{},\"info\":\"{}\",\"cosyVersion\":\"{}\",\"ideVersion\":\"\"}}",
            json_string(request_id),
            self.info,
            COSY_VERSION
        );
        let payload_b64 = BASE64.encode(payload.as_bytes());
        let path = sign_path(raw_url)?;
        let digest = cosy_signature(&payload_b64, &self.cosy_key, date, body, &path);
        Ok(CosySignedRequest {
            authorization: format!("Bearer COSY.{payload_b64}.{digest}"),
            payload_b64,
            date: date.to_string(),
            path,
        })
    }

    /// Headers for the streaming chat endpoint.
    ///
    /// Only the four signed headers plus a fixed User-Agent are required; the
    /// live probe showed the `Cosy-ClientType`/`Cosy-Machine*` family is not.
    pub fn chat_headers(
        &self,
        encoded_body: &str,
        raw_url: &str,
        model_key: &str,
        date: &str,
        request_id: &str,
    ) -> Result<Vec<(String, String)>, GatewayError> {
        let signed = self.sign(encoded_body, raw_url, date, request_id)?;
        let mut headers = self.base_headers(&signed, "text/event-stream");
        headers.push(("cache-control".into(), "no-cache".into()));
        if !model_key.is_empty() {
            headers.push(("x-model-key".into(), model_key.to_string()));
            headers.push(("x-model-source".into(), "system".into()));
        }
        Ok(headers)
    }

    /// Headers for the model list. The signature body is `qoder_encode("")`.
    pub fn models_headers(
        &self,
        sign_body: &str,
        raw_url: &str,
        date: &str,
        request_id: &str,
    ) -> Result<Vec<(String, String)>, GatewayError> {
        let signed = self.sign(sign_body, raw_url, date, request_id)?;
        Ok(self.base_headers(&signed, "application/json"))
    }

    fn base_headers(&self, signed: &CosySignedRequest, accept: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("cosy-user".into(), self.uid.clone()),
            ("cosy-key".into(), self.cosy_key.clone()),
            ("cosy-date".into(), signed.date.clone()),
            ("authorization".into(), signed.authorization.clone()),
            ("content-type".into(), "application/json".into()),
            ("accept".into(), accept.into()),
            ("accept-encoding".into(), "identity".into()),
            ("user-agent".into(), CLIENT_UA.into()),
        ];
        // The client-identity family is not needed for the signature to verify,
        // but the upstream bills/routes the caller as an unknown device without
        // it. `cosy-user` is already above; the rest comes from the profile.
        if self.identity.is_impersonating() {
            headers.extend(
                profile::qoder_cosy_headers(&self.uid, COSY_VERSION)
                    .into_iter()
                    .filter(|(name, _)| *name != "cosy-user")
                    .map(|(name, value)| (name.to_string(), value)),
            );
        }
        headers
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a &str always serializes")
}

fn server_public_key() -> Result<&'static RsaPublicKey, GatewayError> {
    static KEY: OnceLock<RsaPublicKey> = OnceLock::new();
    if let Some(key) = KEY.get() {
        return Ok(key);
    }
    let parsed = RsaPublicKey::from_public_key_pem(SERVER_PUBLIC_KEY_PEM)
        .map_err(|_| GatewayError::internal("invalid embedded Qoder public key"))?;
    Ok(KEY.get_or_init(|| parsed))
}

fn aes_cbc_encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let cipher = Aes128CbcEnc::new(key.into(), key.into());
    cipher.encrypt_padded_vec_mut::<Pkcs7>(plain)
}

/// Encrypt/decrypt helpers used by tests. Kept private so production callers
/// cannot accidentally treat the identity blob as general crypto.
#[cfg(test)]
pub(crate) fn aes_cbc_decrypt(ciphertext: &[u8], key: &[u8; 16]) -> Vec<u8> {
    use aes::cipher::BlockDecryptMut;
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    Aes128CbcDec::new(key.into(), key.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .expect("valid PKCS7 padding")
}

/// Qoder's custom Base64 variant:
/// standard Base64 -> rotate the tail/middle/head -> map to a custom alphabet
/// -> `=` becomes `$`.
pub fn qoder_encode(plain: &[u8]) -> String {
    let standard = BASE64.encode(plain);
    let size = standard.len();
    let head = size / 3;
    let rearranged = if size == 0 {
        String::new()
    } else {
        format!(
            "{}{}{}",
            &standard[size - head..],
            &standard[head..size - head],
            &standard[..head]
        )
    };
    rearranged.chars().map(encode_char).collect()
}

/// Inverse of [`qoder_encode`], used by tests and diagnostics.
pub fn qoder_decode(encoded: &str) -> Result<Vec<u8>, GatewayError> {
    let mut standard = String::with_capacity(encoded.len());
    for ch in encoded.chars() {
        standard.push(decode_char(ch)?);
    }
    let size = standard.len();
    let head = size / 3;
    let (r1, rest) = standard.split_at(head);
    let (r2, r3) = rest.split_at(size - head * 2);
    let original = format!("{r3}{r2}{r1}");
    BASE64
        .decode(original.as_bytes())
        .map_err(|_| GatewayError::bad_request("invalid Qoder COSY payload"))
}

fn encode_char(ch: char) -> char {
    match STANDARD_ALPHABET.find(ch) {
        Some(index) => CUSTOM_ALPHABET
            .chars()
            .nth(index)
            .expect("alphabets have equal length"),
        None if ch == '=' => CUSTOM_PAD,
        None => ch,
    }
}

fn decode_char(ch: char) -> Result<char, GatewayError> {
    if ch == CUSTOM_PAD {
        return Ok('=');
    }
    CUSTOM_ALPHABET
        .find(ch)
        .map(|index| {
            STANDARD_ALPHABET
                .chars()
                .nth(index)
                .expect("alphabets have equal length")
        })
        .ok_or_else(|| GatewayError::bad_request("invalid Qoder COSY payload"))
}

/// Stable OpenAPI request identity derived from the uid.
pub fn derive_id(uid: &str, salt: &str) -> String {
    let uid = if uid.is_empty() { "anonymous" } else { uid };
    hex::encode(Md5::digest(format!("{salt}:{uid}").as_bytes()))
}

/// URL path used in the signature: path only, `/algo` prefix removed, query
/// string excluded.
pub fn sign_path(raw_url: &str) -> Result<String, GatewayError> {
    let url = url::Url::parse(raw_url)
        .map_err(|_| GatewayError::internal("invalid Qoder request URL"))?;
    let mut path = url.path().to_string();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(stripped) = path.strip_prefix("/algo") {
        path = stripped.to_string();
    }
    Ok(path)
}

pub fn cosy_signature(
    payload_b64: &str,
    cosy_key: &str,
    date: &str,
    body: &str,
    path: &str,
) -> String {
    let raw = [payload_b64, cosy_key, date, body, path].join("\n");
    hex::encode(Md5::digest(raw.as_bytes()))
}

/// COSY `requestId`: uuid v4 without dashes, like the official clients.
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// COSY `date` value: Unix seconds as a decimal string (`int(time.time())`).
pub fn cosy_date(now: chrono::DateTime<chrono::Utc>) -> String {
    now.timestamp().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CosyIdentity {
        CosyIdentity::new("u9", "tok").with_name("N")
    }

    #[test]
    fn custom_base64_matches_fixed_vectors() {
        assert_eq!(qoder_encode(b""), "");
        assert_eq!(qoder_encode(b"{\"a\":1}"), "ep$$BMn%mYKi");
        assert!(qoder_encode(b"\x00").contains(CUSTOM_PAD));
        assert!(!qoder_encode(b"\x00").contains(['+', '/', '=']));
        for plain in [&b""[..], b"a", b"ab", b"abc", b"hello world!"] {
            assert_eq!(qoder_decode(&qoder_encode(plain)).unwrap(), plain);
        }
    }

    #[test]
    fn derived_ids_match_fixed_vectors() {
        assert_eq!(
            derive_id("u1", "machine"),
            "1c77b9ac7fdc6e92cc94595f68a05717"
        );
        assert_eq!(derive_id("", "machine"), derive_id("anonymous", "machine"));
    }

    #[test]
    fn signature_matches_fixed_vector() {
        assert_eq!(
            cosy_signature(
                "p",
                "k",
                "1700000000",
                "BODY",
                "/api/v2/service/pro/sse/agent_chat_generation"
            ),
            "8b28c0f19e7491258363d76110c86a8c"
        );
    }

    #[test]
    fn sign_path_strips_algo_and_query() {
        assert_eq!(
            sign_path("https://gateway.qoder.com.cn/algo/api/v2/model/list?Encode=1").unwrap(),
            "/api/v2/model/list"
        );
        assert_eq!(
            sign_path(
                "https://api1.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?x=1"
            )
            .unwrap(),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
        assert_eq!(sign_path("https://gw.test/other").unwrap(), "/other");
    }

    #[test]
    fn info_blob_matches_fixed_aes_vector() {
        let session = CosySession::new(identity(), *b"0123456789abcdef").unwrap();
        assert_eq!(
            session.info(),
            "GKFTcx754I9xjUsnRwL67ogoS3mWEFPa+c4QXU9Xqp5UaEq2Q7Y7SEErUBIxNdFuHd2iU9NLfiszRExhND0VIlST1iYOogjw17hg5wQGcAk="
        );
        let plain = aes_cbc_decrypt(&BASE64.decode(session.info()).unwrap(), b"0123456789abcdef");
        let text = String::from_utf8(plain).unwrap();
        assert_eq!(
            text,
            "{\"uid\":\"u9\",\"aid\":\"\",\"name\":\"N\",\"email\":\"\",\"security_oauth_token\":\"tok\"}"
        );
    }

    #[test]
    fn rsa_wrap_roundtrips_with_a_test_key() {
        use rsa::rand_core::OsRng;
        use rsa::RsaPrivateKey;
        let private = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let public = RsaPublicKey::from(&private);
        let session =
            CosySession::with_public_key(identity(), *b"0123456789abcdef", &public).unwrap();
        let wrapped = BASE64.decode(session.cosy_key()).unwrap();
        let key = private.decrypt(Pkcs1v15Encrypt, &wrapped).unwrap();
        assert_eq!(key, b"0123456789abcdef");
    }

    #[test]
    fn date_is_unix_seconds() {
        let now = chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(cosy_date(now), "1700000000");
    }

    #[test]
    fn payload_envelope_keeps_the_verified_field_order() {
        let session = CosySession::new(identity(), *b"0123456789abcdef").unwrap();
        let signed = session
            .sign("BODY", "https://gw.test/algo/x?y=1", "1700000000", "rid")
            .unwrap();
        let decoded = BASE64.decode(&signed.payload_b64).unwrap();
        let text = String::from_utf8(decoded).unwrap();
        assert!(text.starts_with("{\"version\":\"v1\",\"requestId\":\"rid\",\"info\":\""));
        assert!(text.ends_with(&format!(
            "\",\"cosyVersion\":\"{COSY_VERSION}\",\"ideVersion\":\"\"}}"
        )));
    }

    #[test]
    fn headers_carry_only_the_signed_fields_and_redact_debug() {
        let session = CosySession::new(identity(), *b"0123456789abcdef").unwrap();
        let url =
            "https://gateway.qoder.com.cn/algo/api/v2/service/pro/sse/agent_chat_generation?x=1";
        let headers = session
            .chat_headers("BODY", url, "qmodel_38max", "1700000000", "rid")
            .unwrap();
        let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "cosy-user",
                "cosy-key",
                "cosy-date",
                "authorization",
                "content-type",
                "accept",
                "accept-encoding",
                "user-agent",
                "cache-control",
                "x-model-key",
                "x-model-source",
            ]
        );
        assert!(headers[3].1.starts_with("Bearer COSY."));
        assert!(!format!("{session:?}").contains("tok"));
        assert!(!format!("{session:?}").contains(session.info()));
        assert!(!format!("{session:?}").contains(session.cosy_key()));
    }
}
