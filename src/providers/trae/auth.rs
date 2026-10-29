//! TRAE authorization and token exchange.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §3-§4; Coding2API
//! `src/provider/trae/callback.py:20-143` (login URL, callback parsing) and
//! `client.py:478-500` (`refresh_token`, `get_user_info`).
//!
//! The browser callback carries the refresh token; the gateway never receives a
//! password. The callback URL is parsed locally and the refresh token is
//! exchanged for an access token at the official OAuth host — the same call
//! that implements refresh.
//!
//! Compliance boundary: the reference generates a fresh hex32 machine/device
//! identity for each login. This adapter **does not generate** device
//! identifiers; the operator supplies the pair, which is then persisted with the
//! credential so refresh and login stay consistent. Without them the flow is
//! refused instead of fabricating a device identity.
//!
//! International login is not implemented: neither reference contains a
//! verified International authorization host. A credential imported with an
//! International `apiHost` can still be refreshed through it.

use std::time::Duration;

use chrono::Utc;
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Value};

use crate::config::Region;
use crate::error::GatewayError;

use super::credential::TraeCredential;
use super::error::{classify_http_status, classify_transport};
use super::region::{self, OAUTH_EXCHANGE_PATH, OAUTH_USER_INFO_PATH};

pub const LOGIN_HOST: &str = "https://www.trae.cn";
pub const LOGIN_PATH: &str = "/authorization";
/// Public OAuth client id used by TRAE's own desktop client. It is not a secret
/// and is required by `ExchangeToken`; it is not a device identifier.
pub const CLIENT_ID: &str = "en1oxy7wnw8j9n";
pub const LOGIN_VERSION: &str = "1";
pub const PLUGIN_VERSION: &str = "2.3.62834";
pub const IDE_VERSION: &str = "0.1.52";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: usize = 256 * 1024;
const CLIENT_UA: &str = concat!("polygate/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraeAuthStart {
    pub auth_url: String,
    /// The value the callback echoes back (`loginTraceID`), derived from the
    /// supplied machine/device pair. Used to bind a pasted callback to this
    /// login attempt.
    pub login_trace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraeCallbackInfo {
    pub refresh_token: String,
    pub uid: String,
    pub nickname: String,
    pub enterprise_id: String,
    pub expires_at: i64,
}

#[derive(Clone)]
pub struct TraeAuthClient {
    client: reqwest::Client,
    base_override: Option<String>,
}

impl std::fmt::Debug for TraeAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraeAuthClient")
            .field("base_override", &self.base_override)
            .finish_non_exhaustive()
    }
}

impl TraeAuthClient {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_override: None,
        }
    }

    /// Tests only: point ExchangeToken / GetUserInfo at one mock server.
    pub fn with_base(client: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            client,
            base_override: Some(base.into().trim_end_matches('/').to_string()),
        }
    }

    /// Build the official login URL. No network call is made.
    pub fn start(
        &self,
        region: Region,
        machine_id: &str,
        device_id: &str,
        callback_url: &str,
    ) -> Result<TraeAuthStart, GatewayError> {
        if region != Region::Cn {
            return Err(GatewayError::unsupported(
                "TRAE International login has no verified authorization host",
            ));
        }
        if machine_id.trim().is_empty() || device_id.trim().is_empty() {
            return Err(GatewayError::bad_request(
                "TRAE login needs operator-supplied machineId and deviceId; \
                 PolyGate does not generate device identities",
            ));
        }
        let callback_url = callback_url.trim();
        let parsed = url::Url::parse(callback_url).map_err(|_| {
            GatewayError::bad_request("TRAE login callback URL is not a valid absolute URL")
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(GatewayError::bad_request(
                "TRAE login callback URL must be http(s)",
            ));
        }
        Ok(TraeAuthStart {
            auth_url: build_login_url(callback_url, machine_id, device_id),
            login_trace_id: login_trace_id(machine_id, device_id),
        })
    }

    /// Complete the flow from the copied callback URL: parse → ExchangeToken →
    /// GetUserInfo when the callback carried no uid.
    pub async fn complete(
        &self,
        region: Region,
        raw_url: &str,
        machine_id: &str,
        device_id: &str,
    ) -> Result<TraeCredential, GatewayError> {
        let info = parse_callback_url(raw_url)?;
        let base = self.oauth_base(region, "")?;
        let exchanged = exchange_refresh_token(&self.client, &base, &info.refresh_token).await?;
        let skeleton = TraeCredential {
            access_token: String::new(),
            refresh_token: info.refresh_token.clone(),
            uid: info.uid.clone(),
            domain: region::realm(region).default_domain.to_string(),
            api_host: String::new(),
            machine_id: machine_id.trim().to_string(),
            device_id: device_id.trim().to_string(),
            enterprise_id: info.enterprise_id.clone(),
            nickname: info.nickname.clone(),
            expires_at: 0,
            region,
        };
        let mut credential = skeleton.merge_refreshed(&exchanged, Utc::now().timestamp())?;
        if credential.uid.is_empty() {
            let (uid, nickname) =
                get_user_info(&self.client, &base, &credential.access_token).await?;
            credential.uid = uid;
            if credential.nickname.is_empty() {
                credential.nickname = nickname;
            }
        }
        if credential.uid.is_empty() {
            return Err(GatewayError::auth_invalid(
                "TRAE returned no user id for this credential",
            ));
        }
        Ok(credential)
    }

    fn oauth_base(&self, region: Region, api_host: &str) -> Result<String, GatewayError> {
        if let Some(base) = self.base_override.as_deref() {
            return Ok(base.to_string());
        }
        resolve_oauth_base(region, api_host)
    }
}

/// Use the credential's validated `apiHost` when present, otherwise the realm's
/// documented OAuth host. An International credential with neither has no
/// verified endpoint and is refused instead of guessing CN's.
pub fn resolve_oauth_base(region: Region, api_host: &str) -> Result<String, GatewayError> {
    let host = api_host.trim().trim_end_matches('/');
    if !host.is_empty() {
        return Ok(host.to_string());
    }
    let base = region::realm(region).oauth_base;
    if base.is_empty() {
        return Err(GatewayError::unsupported(
            "TRAE International token exchange host is not verified; \
             import the credential's apiHost or refresh it manually",
        ));
    }
    Ok(base.to_string())
}

/// `login_trace_id` = the last 16 characters of `machine_id + device_id`.
/// TRAE echoes it as `loginTraceID` on the callback URL.
pub fn login_trace_id(machine_id: &str, device_id: &str) -> String {
    let combined = format!("{machine_id}{device_id}");
    let chars: Vec<char> = combined.chars().collect();
    let start = chars.len().saturating_sub(16);
    chars[start..].iter().collect()
}

/// Build the official authorization URL. Query order follows the reference so
/// the upstream sees the same parameter set.
pub fn build_login_url(callback_url: &str, machine_id: &str, device_id: &str) -> String {
    let trace_id = login_trace_id(machine_id, device_id);
    let machine = encode(machine_id);
    let device = encode(device_id);
    let callback = encode(callback_url);
    format!(
        "{LOGIN_HOST}{LOGIN_PATH}?login_version={LOGIN_VERSION}\
         &auth_from=solo\
         &login_channel=native_ide\
         &plugin_version={PLUGIN_VERSION}\
         &auth_type=local\
         &client_id={CLIENT_ID}\
         &redirect=0\
         &auth_callback_url={callback}\
         &login_trace_id={trace_id}\
         &machine_id={machine}\
         &device_id={device}\
         &x_device_id={device}\
         &x_machine_id={machine}\
         &x_device_brand=PC\
         &x_device_type=PC\
         &x_os_version=1.0\
         &x_app_version={IDE_VERSION}\
         &x_app_type=stable"
    )
}

/// `ExchangeToken` returns the token material in `Result`. Callers either merge
/// it into a credential or use it directly for `GetUserInfo`.
pub async fn exchange_refresh_token(
    client: &reqwest::Client,
    base: &str,
    refresh_token: &str,
) -> Result<Value, GatewayError> {
    if refresh_token.trim().is_empty() {
        return Err(GatewayError::auth_invalid("TRAE refresh token is empty"));
    }
    let url = format!("{}{}", base.trim_end_matches('/'), OAUTH_EXCHANGE_PATH);
    let body = json!({
        "ClientID": CLIENT_ID,
        "RefreshToken": refresh_token,
        "ClientSecret": "-",
        "UserID": "",
    });
    let payload = post_oauth(client, &url, &body, None).await?;
    let result = payload
        .get("Result")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            GatewayError::upstream_server("TRAE token exchange returned no Result object")
        })?;
    if result.get("Token").and_then(Value::as_str).is_none() {
        return Err(GatewayError::auth_invalid(
            "TRAE token exchange returned no access token",
        ));
    }
    Ok(Value::Object(result.clone()))
}

pub async fn get_user_info(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
) -> Result<(String, String), GatewayError> {
    let url = format!("{}{}", base.trim_end_matches('/'), OAUTH_USER_INFO_PATH);
    let payload = post_oauth(
        client,
        &url,
        &json!({"ReqSource": "IDE"}),
        Some(access_token),
    )
    .await?;
    let result = payload
        .get("Result")
        .and_then(Value::as_object)
        .ok_or_else(|| GatewayError::upstream_server("TRAE user info returned no Result object"))?;
    Ok((
        result
            .get("UserID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        result
            .get("ScreenName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
    ))
}

async fn post_oauth(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    access_token: Option<&str>,
) -> Result<Value, GatewayError> {
    let mut request = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, CLIENT_UA)
        .json(body)
        .timeout(REQUEST_TIMEOUT);
    if let Some(token) = access_token.filter(|token| !token.is_empty()) {
        let value = HeaderValue::from_str(token)
            .map_err(|_| GatewayError::auth_invalid("TRAE token cannot be sent in a header"))?;
        request = request.header("X-Cloudide-Token", value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| classify_transport(&error))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| classify_transport(&error))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(GatewayError::upstream_server(
            "TRAE OAuth response exceeded the size limit",
        ));
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    if !(200..300).contains(&status) {
        // 401/403 from a token exchange means the refresh token is dead; that is
        // terminal, not an upstream outage.
        let error = if status == 401 || status == 403 {
            GatewayError::auth_invalid(
                "TRAE rejected the refresh token; authorize the account again",
            )
            .with_upstream_status(status)
        } else {
            classify_http_status(status, &body)
        };
        return Err(error);
    }
    serde_json::from_str::<Value>(&body).map_err(|_| {
        GatewayError::upstream_server("TRAE OAuth returned unparsable JSON")
            .with_upstream_status(status)
    })
}

/// Parse the URL TRAE redirects to. Only the query string is read; the call is
/// never followed.
pub fn parse_callback_url(raw_url: &str) -> Result<TraeCallbackInfo, GatewayError> {
    let raw_url = raw_url.trim();
    if raw_url.is_empty() {
        return Err(GatewayError::bad_request("TRAE callback URL is empty"));
    }
    let url = url::Url::parse(raw_url)
        .map_err(|_| GatewayError::bad_request("TRAE callback URL is not a valid URL"))?;
    let mut refresh_token = String::new();
    let mut user_info = Value::Null;
    let mut user_jwt = Value::Null;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "refreshToken" if refresh_token.is_empty() => refresh_token = value.into_owned(),
            "userInfo" => user_info = json_param(&value),
            "userJwt" => user_jwt = json_param(&value),
            _ => {}
        }
    }
    if refresh_token.trim().is_empty() {
        refresh_token = string_at(&user_jwt, "RefreshToken");
        if refresh_token.is_empty() {
            refresh_token = string_at(&user_jwt, "Token");
        }
    }
    if refresh_token.trim().is_empty() {
        return Err(GatewayError::bad_request(
            "TRAE callback carries no refreshToken",
        ));
    }
    Ok(TraeCallbackInfo {
        refresh_token: refresh_token.trim().to_string(),
        uid: first_of(&user_info, &["uid", "UID"]),
        nickname: first_of(&user_info, &["nickname", "ScreenName"]),
        enterprise_id: first_of(&user_info, &["enterpriseId", "EnterpriseID"]),
        expires_at: int_at(&user_info, "expiresAt")
            .map(normalize_epoch)
            .unwrap_or(0),
    })
}

fn json_param(raw: &str) -> Value {
    let mut text = raw.to_string();
    for _ in 0..3 {
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::String(inner)) => text = inner,
            Ok(value) => return value,
            Err(_) => {
                let decoded = percent_decode(&text);
                if decoded == text {
                    break;
                }
                text = decoded;
            }
        }
    }
    Value::Null
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_epoch(value: i64) -> i64 {
    if value > 100_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn first_of(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_at(value, key))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

fn int_at(value: &Value, key: &str) -> Option<i64> {
    match value.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::test_support::{spawn_server, Reply};

    const MACHINE: &str = "0123456789abcdef0123456789abcdef";
    const DEVICE: &str = "fedcba9876543210fedcba9876543210";

    fn callback_url() -> String {
        let info = json!({"uid": "uid-1", "nickname": "nick", "enterpriseId": "ent-1"});
        format!(
            "http://127.0.0.1:8787/oauth/callback?refreshToken=refresh-1&userInfo={}",
            encode(&info.to_string())
        )
    }

    #[test]
    fn login_url_carries_the_supplied_identity() {
        let url = build_login_url("http://127.0.0.1:8787/oauth/callback", MACHINE, DEVICE);
        assert!(url.starts_with("https://www.trae.cn/authorization?"));
        assert!(url.contains("auth_from=solo"));
        assert!(url.contains(&format!("machine_id={MACHINE}")));
        assert!(url.contains(&format!("device_id={DEVICE}")));
        assert!(url.contains(&format!("x_machine_id={MACHINE}")));
        assert!(url.contains("auth_callback_url=http%3A%2F%2F127.0.0.1%3A8787%2Foauth%2Fcallback"));
        assert!(url.contains(&format!(
            "login_trace_id={}",
            login_trace_id(MACHINE, DEVICE)
        )));
    }

    #[test]
    fn login_trace_is_the_last_sixteen_characters() {
        assert_eq!(login_trace_id(MACHINE, DEVICE).len(), 16);
        assert_eq!(login_trace_id("a", "b"), "ab");
    }

    #[test]
    fn callback_parsing_accepts_encoded_json() {
        let info = parse_callback_url(&callback_url()).unwrap();
        assert_eq!(info.refresh_token, "refresh-1");
        assert_eq!(info.uid, "uid-1");
        assert_eq!(info.nickname, "nick");
        assert_eq!(info.enterprise_id, "ent-1");
    }

    #[test]
    fn callback_parsing_falls_back_to_user_jwt() {
        let jwt = json!({"Token": "access-from-jwt", "RefreshToken": "refresh-from-jwt"});
        let url = format!("http://x/cb?userJwt={}", encode(&jwt.to_string()));
        let info = parse_callback_url(&url).unwrap();
        assert_eq!(info.refresh_token, "refresh-from-jwt");
    }

    #[test]
    fn a_callback_without_a_refresh_token_is_a_bad_request() {
        let error = parse_callback_url("http://x/cb?foo=1").unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadRequest);
    }

    #[test]
    fn international_login_is_unsupported() {
        let client = TraeAuthClient::new(reqwest::Client::new());
        let error = client
            .start(
                Region::Intl,
                MACHINE,
                DEVICE,
                "http://127.0.0.1:8787/oauth/callback",
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn completion_exchanges_and_fills_missing_identity() {
        let server = spawn_server(|request| match request.path() {
            "/cloudide/api/v3/trae/oauth/ExchangeToken" => Reply::json(
                200,
                json!({
                    "Result": {
                        "Token": "access-1",
                        "RefreshToken": "refresh-2",
                        "TokenExpireAt": 1_900_000_000_000_i64
                    }
                })
                .to_string(),
            ),
            "/cloudide/api/v3/trae/GetUserInfo" => Reply::json(
                200,
                json!({"Result": {"UserID": "uid-9", "ScreenName": "nine"}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let client = TraeAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let url = "http://x/cb?refreshToken=refresh-1&userInfo=%7B%7D";
        let credential = client
            .complete(Region::Cn, url, MACHINE, DEVICE)
            .await
            .unwrap();
        assert_eq!(credential.access_token, "access-1");
        assert_eq!(credential.refresh_token, "refresh-2");
        assert_eq!(credential.uid, "uid-9");
        assert_eq!(credential.nickname, "nine");
        assert_eq!(credential.machine_id, MACHINE);
        assert_eq!(credential.device_id, DEVICE);
        assert_eq!(credential.expires_at, 1_900_000_000);
        assert_eq!(server.request_count(), 2);
        assert!(server.requests()[1]
            .header("x-cloudide-token")
            .is_some_and(|value| value == "access-1"));
    }

    #[tokio::test]
    async fn a_rejected_exchange_is_terminal_and_sends_no_user_info_call() {
        let server = spawn_server(|_| Reply::json(401, "{}")).await;
        let client = TraeAuthClient::with_base(reqwest::Client::new(), &server.base_url);
        let error = client
            .complete(
                Region::Cn,
                "http://x/cb?refreshToken=refresh-1&userInfo=%7B%7D",
                MACHINE,
                DEVICE,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
        assert_eq!(server.request_count(), 1);
    }
}
