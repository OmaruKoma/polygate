//! China Mobile "Suanlidou" (算力豆) provider — balance probe only.
//!
//! Evidence: `qodercmcc.trace` (Qoder CN, 2026-11-01) plus a live read-only
//! replay of the three order-center calls. The desktop links the Qoder CMCC
//! channel to the China Mobile order center; the pages there are the only place
//! that reports the 算力豆 balance.
//!
//! Facts this adapter depends on:
//! * Qoder CN `GET /api/v2/quota/usage?outerProviders=cmcc` returns an
//!   `outerProviders[].usageLogLink` whose `token` query value is an SSO token
//!   (`sso_agent_...`). The Qoder credit numbers in that response are **not**
//!   the 算力豆 balance — they matched the non-CMCC view exactly.
//! * That link token is short-lived (the one captured on 2026-11-01 answered
//!   3004 late the same day), so a stored Qoder `access_token` — re-exchanged
//!   for a fresh link on every probe — is the durable credential.
//! * `POST {base}/client/v1/ssoLogin` with `{"sso_token": ...}` answers
//!   `code: 0` plus an `order_session`; a bad token answers `3004`.
//! * `POST {base}/client/v1/token/usage` with the session reports
//!   `userTotalBalance` / `userRemainBalance` in raw units; the order-center
//!   page divides by 10 000 before showing 算力豆.
//! * `POST {base}/client/v1/cmt/balance` is the older CMT ledger and answered
//!   with no resources on the captured account, so it is not used.
//!
//! The provider is balance-only: no check-in and no chat. It never writes.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::accounts::Credential;
use crate::config::{ProviderKind, Region, Verification};
use crate::error::{ErrorKind, GatewayError};
use crate::providers::{Capabilities, Provider, ProviderInfo};
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

/// Order-center root (the page's `apiBase()`).
pub const DEFAULT_BASE: &str = "https://ai.soho.komect.com/ai/llm/order-center/public";
/// Qoder CN endpoint that hands out the SSO link.
pub const QODER_QUOTA_URL: &str =
    "https://openapi.qoder.com.cn/api/v2/quota/usage?outerProviders=cmcc";
/// Raw balances are 10 000 units per 算力豆 (order-center `tokenToBeanValue`).
const RAW_PER_BEAN: f64 = 10_000.0;
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub struct SuanlidouProvider {
    info: ProviderInfo,
    client: reqwest::Client,
    base: String,
    qoder_quota_url: String,
}

impl std::fmt::Debug for SuanlidouProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuanlidouProvider")
            .field("id", &self.info.id)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl SuanlidouProvider {
    pub fn new(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
    ) -> Self {
        Self::build(
            id,
            region,
            verification,
            client,
            DEFAULT_BASE.to_string(),
            QODER_QUOTA_URL.to_string(),
        )
    }

    /// Tests only: point both the order center and the Qoder probe at mocks.
    pub fn with_base(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: impl Into<String>,
        qoder_quota_url: impl Into<String>,
    ) -> Self {
        Self::build(
            id,
            region,
            verification,
            client,
            base.into(),
            qoder_quota_url.into(),
        )
    }

    fn build(
        id: impl Into<String>,
        region: Region,
        verification: Verification,
        client: reqwest::Client,
        base: String,
        qoder_quota_url: String,
    ) -> Self {
        let info = ProviderInfo {
            id: id.into(),
            kind: ProviderKind::Suanlidou,
            region,
            display_name: ProviderKind::Suanlidou.display_name().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            verification: verification.into(),
            enabled: true,
        };
        Self {
            info,
            client,
            base: base.trim_end_matches('/').to_string(),
            qoder_quota_url,
        }
    }

    fn text(credential: &Credential, key: &str) -> String {
        credential
            .to_json()
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string()
    }

    /// Resolve the order-center SSO token: either stored directly, or derived
    /// from the Qoder CN access token's CMCC usage link.
    async fn sso_token(&self, credential: &Credential) -> Result<String, GatewayError> {
        let direct = Self::text(credential, "sso_token");
        if !direct.is_empty() {
            return Ok(direct);
        }
        let access = Self::text(credential, "access_token");
        if access.is_empty() {
            return Err(GatewayError::auth_invalid(
                "Suanlidou needs `sso_token`, or a Qoder CN `access_token` that can be \
                 exchanged for the CMCC usage link",
            ));
        }
        let response = self
            .client
            .get(&self.qoder_quota_url)
            .header("authorization", format!("Bearer {access}"))
            .header("cosy-clienttype", "10")
            .header("accept", "application/json")
            .header("user-agent", "Qoder")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| {
                GatewayError::transport(format!("Qoder quota probe failed: {error}"))
            })?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|_| GatewayError::upstream_server("Qoder quota probe returned no JSON"))?;
        if !status.is_success() {
            return Err(crate::providers::qoder::error::classify_http_status(
                status.as_u16(),
                &payload.to_string(),
            ));
        }
        payload
            .get("outerProviders")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("usageLogLink"))
            .and_then(Value::as_str)
            .and_then(link_token)
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::Unsupported,
                    "this Qoder account has no CMCC partner link; authorize the China Mobile plan first",
                )
            })
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, GatewayError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .client
            .post(&url)
            .json(&body)
            .header("accept", "application/json")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| {
                GatewayError::transport(format!("Suanlidou request failed: {error}"))
            })?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .map_err(|_| GatewayError::upstream_server("Suanlidou returned no JSON"))?;
        if status.as_u16() == 401 {
            return Err(GatewayError::new(
                ErrorKind::AuthExpired,
                "Suanlidou order session expired",
            ));
        }
        if !status.is_success() {
            return Err(GatewayError::upstream_server(format!(
                "Suanlidou answered HTTP {status}"
            )));
        }
        let code = payload.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            // 3004: the SSO token was rejected; the stored credential is dead.
            let kind = if code == 3004 {
                ErrorKind::AuthInvalid
            } else {
                ErrorKind::UpstreamServer
            };
            return Err(GatewayError::new(
                kind,
                format!("Suanlidou error {code}: {message}"),
            ));
        }
        Ok(payload.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Exchange the SSO token and read the balance; returns 算力豆 (total,
    /// remaining) after the page's own /10 000 conversion.
    pub async fn balance(&self, credential: &Credential) -> Result<(f64, f64), GatewayError> {
        let token = self.sso_token(credential).await?;
        let login = self
            .post("/client/v1/ssoLogin", json!({ "sso_token": token }))
            .await?;
        let session = login
            .get("order_session")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| GatewayError::auth_invalid("Suanlidou login returned no order session"))?
            .to_string();
        let usage = self
            .post(
                "/client/v1/token/usage",
                json!({
                    "order_session": session,
                    "page": 1,
                    "pageNum": 1,
                    "page_size": 20,
                }),
            )
            .await?;
        let total = raw_balance(&usage, "userTotalBalance");
        let remaining = raw_balance(&usage, "userRemainBalance");
        match (total, remaining) {
            (Some(total), Some(remaining)) => Ok((total / RAW_PER_BEAN, remaining / RAW_PER_BEAN)),
            _ => Err(GatewayError::upstream_server(
                "Suanlidou usage response has no userTotalBalance/userRemainBalance",
            )),
        }
    }
}

/// Balances arrive as JSON strings (`"9500000"`), so accept both spellings.
fn raw_balance(payload: &Value, key: &str) -> Option<f64> {
    match payload.get(key)? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// `token` query value of the order-center usage link.
fn link_token(link: &str) -> Option<String> {
    let query = link.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "token" && !value.is_empty()).then(|| value.to_string())
    })
}

#[async_trait]
impl Provider for SuanlidouProvider {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            authentication: false,
            model_discovery: false,
            quota: true,
            chat: false,
            streaming: false,
            tools: false,
            native_session: false,
            agent_task: false,
            cancellation: false,
        }
    }

    async fn quota(&self, credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        let (total, remaining) = self.balance(credential).await?;
        Ok(QuotaSnapshot {
            account_id: String::new(),
            group_id: None,
            model: None,
            unit: QuotaUnit::Beans,
            total: Some(total),
            remaining: Some(remaining.max(0.0)),
            status: QuotaStatus::Known,
            error: None,
            refreshed_at: chrono::Utc::now(),
            raw: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{spawn_server, Reply};

    fn provider(base: &str) -> SuanlidouProvider {
        SuanlidouProvider::with_base(
            "suanlidou-cn",
            Region::Cn,
            Verification::Experimental,
            reqwest::Client::new(),
            base,
            format!("{base}/qoder/quota"),
        )
    }

    fn credential(value: Value) -> Credential {
        Credential::from_value(value)
    }

    #[test]
    fn link_token_is_read_from_the_query_string() {
        assert_eq!(
            link_token("https://x/pages/token-usage?business=qoder&token=sso_agent_1"),
            Some("sso_agent_1".into())
        );
        assert_eq!(link_token("https://x/none"), None);
    }

    #[tokio::test]
    async fn a_stored_sso_token_is_exchanged_and_the_balance_is_scaled() {
        let server = spawn_server(|request| match request.path() {
            "/client/v1/ssoLogin" => Reply::json(
                200,
                json!({"code":0,"data":{"order_session":"sess-1","phone":"13600000000"}})
                    .to_string(),
            ),
            "/client/v1/token/usage" => Reply::json(
                200,
                json!({"code":0,"data":{"userTotalBalance":9500000,"userRemainBalance":9429699}})
                    .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;

        let (total, remaining) = provider(&server.base_url)
            .balance(&credential(json!({"sso_token":"sso_agent_1"})))
            .await
            .unwrap();
        assert!((total - 950.0).abs() < 1e-9, "{total}");
        assert!((remaining - 942.9699).abs() < 1e-9, "{remaining}");

        let sent = server.requests();
        assert_eq!(sent[0].path(), "/client/v1/ssoLogin");
        assert!(sent[0].body.contains("\"sso_token\":\"sso_agent_1\""));
        assert_eq!(sent[1].path(), "/client/v1/token/usage");
        assert!(sent[1].body.contains("\"order_session\":\"sess-1\""));
    }

    #[tokio::test]
    async fn a_qoder_access_token_is_exchanged_for_the_cmcc_link() {
        let server = spawn_server(|request| match request.path() {
            "/qoder/quota" => Reply::json(
                200,
                json!({"outerProviders":[{"provider":"cmcc",
                    "usageLogLink":"https://ai.soho.komect.com/x?business=qoder&token=sso_agent_2"}]})
                .to_string(),
            ),
            "/client/v1/ssoLogin" => Reply::json(
                200,
                json!({"code":0,"data":{"order_session":"sess-2"}}).to_string(),
            ),
            "/client/v1/token/usage" => Reply::json(
                200,
                json!({"code":0,"data":{"userTotalBalance":10000,"userRemainBalance":5000}})
                    .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;

        let snapshot = provider(&server.base_url)
            .quota(&credential(json!({"access_token":"dt-x"})))
            .await
            .unwrap();
        assert_eq!(snapshot.unit, QuotaUnit::Beans);
        assert_eq!(snapshot.total, Some(1.0));
        assert_eq!(snapshot.remaining, Some(0.5));
        let sent = server.requests();
        assert_eq!(sent[0].header("authorization"), Some("Bearer dt-x"));
        assert!(sent[1].body.contains("sso_agent_2"));
    }

    #[tokio::test]
    async fn string_encoded_balances_are_parsed_like_numbers() {
        // The live order center quotes both balances: "9500000", not 9500000.
        let server = spawn_server(|request| match request.path() {
            "/client/v1/ssoLogin" => Reply::json(
                200,
                json!({"code":0,"data":{"order_session":"s"}}).to_string(),
            ),
            "/client/v1/token/usage" => Reply::json(
                200,
                json!({"code":0,"data":{"userTotalBalance":"9500000","userRemainBalance":"9429699"}})
                    .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let snapshot = provider(&server.base_url)
            .quota(&credential(json!({"sso_token":"sso_agent_1"})))
            .await
            .unwrap();
        assert_eq!(snapshot.total, Some(950.0));
        assert_eq!(snapshot.remaining, Some(942.9699));
    }

    #[tokio::test]
    async fn a_rejected_token_is_an_auth_failure() {
        let server = spawn_server(|_| {
            Reply::json(
                200,
                json!({"code":3004,"message":"号卡认证 token 校验失败"}).to_string(),
            )
        })
        .await;
        let error = provider(&server.base_url)
            .quota(&credential(json!({"sso_token":"bad"})))
            .await
            .expect_err("rejected");
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }

    #[tokio::test]
    async fn a_missing_balance_is_never_reported_as_zero() {
        let server = spawn_server(|request| match request.path() {
            "/client/v1/ssoLogin" => Reply::json(
                200,
                json!({"code":0,"data":{"order_session":"s"}}).to_string(),
            ),
            "/client/v1/token/usage" => Reply::json(200, json!({"code":0,"data":{}}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let error = provider(&server.base_url)
            .quota(&credential(json!({"sso_token":"sso_agent_1"})))
            .await
            .expect_err("no balance");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }

    #[tokio::test]
    async fn no_credential_material_is_an_auth_error() {
        let error = provider("http://unused.invalid")
            .quota(&credential(json!({})))
            .await
            .expect_err("no credential");
        assert_eq!(error.kind, ErrorKind::AuthInvalid);
    }
}
