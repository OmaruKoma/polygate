//! CodeArts daily check-in executor (`/v1/ops/*`).
//!
//! Evidence: Coding2API `src/provider/codearts/client.py:320-424` and
//! `events.py:400-501`; the local `codearts2api` has **no** check-in code. The
//! two reference endpoint families are not interchangeable; this adapter uses
//! `/v1/ops/delivery` -> `/v1/ops/claim` -> `/v1/ops/confirm` (no `snap-manager`
//! prefix, `channel=IDE`, campaign id 1).
//!
//! The protocol is **not verified against a real account** by this project.
//! Capabilities stay `experimental`. Status semantics observed by Coding2API
//! (three real accounts, 2026-10):
//!
//! * `ELIGIBLE` + `claimable=true` -> claim, then confirm;
//! * `CLAIMED` -> already claimed but unconfirmed; only confirm;
//! * `CONFIRMED` / `CONSUMED` -> done.
//!
//! A transport/timeout failure returns `Err`, which M12 turns into `Unknown`;
//! it is never reported as a definitive failure.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::account_service::AuthorizationContext;
use crate::accounts::Credential;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::ProviderKind;
use crate::error::GatewayError;
use crate::providers::codearts::{CodeArtsCredential, CodeArtsHttp};

const DELIVERY_PATH: &str = "/v1/ops/delivery?channel=IDE";
const CLAIM_PATH: &str = "/v1/ops/claim";
const CONFIRM_PATH: &str = "/v1/ops/confirm";
const DAILY_CAMPAIGN_ID: i64 = 1;

pub struct CodeArtsCheckinExecutor {
    http: CodeArtsHttp,
}

impl CodeArtsCheckinExecutor {
    pub fn new(
        base_url: impl Into<String>,
        sts_url: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        Ok(Self {
            http: CodeArtsHttp::new(base_url, sts_url)?,
        })
    }

    pub fn with_http(http: CodeArtsHttp) -> Self {
        Self { http }
    }

    fn credential(
        authorization: &AuthorizationContext,
    ) -> Result<CodeArtsCredential, GatewayError> {
        let credential: &Credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("CodeArts account has no credential"))?;
        CodeArtsCredential::parse(credential)
    }

    async fn campaign(
        &self,
        authorization: &AuthorizationContext,
    ) -> Result<Option<Value>, GatewayError> {
        let credential = Self::credential(authorization)?;
        let payload = self
            .http
            .get_json(DELIVERY_PATH, &credential, Some("PromptCenter"), Utc::now())
            .await?;
        Ok(daily_campaign(&payload).cloned())
    }

    async fn claim(
        &self,
        authorization: &AuthorizationContext,
    ) -> Result<(i64, String), GatewayError> {
        let credential = Self::credential(authorization)?;
        let key = format!(
            "claim_{}_{}",
            DAILY_CAMPAIGN_ID,
            Utc::now().timestamp_millis()
        );
        let payload = self
            .http
            .post_json(
                CLAIM_PATH,
                &json!({
                    "campaignId": DAILY_CAMPAIGN_ID,
                    "idempotentKey": key,
                    "channel": "IDE",
                }),
                &credential,
                Utc::now(),
            )
            .await?;
        Ok((business_code(&payload), business_message(&payload)))
    }

    async fn confirm(&self, authorization: &AuthorizationContext) -> Result<(), GatewayError> {
        let credential = Self::credential(authorization)?;
        let payload = self
            .http
            .post_json(
                CONFIRM_PATH,
                &json!({"campaignId": DAILY_CAMPAIGN_ID}),
                &credential,
                Utc::now(),
            )
            .await?;
        let code = business_code(&payload);
        if code != 0 {
            return Err(GatewayError::upstream_server(format!(
                "CodeArts confirm was rejected with code {code}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl CheckinExecutor for CodeArtsCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Codearts
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        let Some(campaign) = self.campaign(authorization).await? else {
            return Ok(CheckinProbe::Unknown);
        };
        Ok(match status(&campaign).as_deref() {
            Some("CONFIRMED") | Some("CONSUMED") => {
                CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
            }
            // ELIGIBLE/CLAIMED do not prove that an interrupted write applied.
            _ => CheckinProbe::Unknown,
        })
    }

    async fn checkin(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let Some(campaign) = self.campaign(authorization).await? else {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "CodeArts daily check-in activity is not available".into(),
            });
        };
        if ended(&campaign) {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "CodeArts daily check-in activity has ended".into(),
            });
        }

        match status(&campaign).as_deref() {
            Some("CONFIRMED") | Some("CONSUMED") => Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "CodeArts reports today as already checked in".into(),
            }),
            Some("CLAIMED") => match self.confirm(authorization).await {
                Ok(()) => Ok(CheckinOutcome::Succeeded {
                    summary: "confirmed a previously claimed CodeArts daily check-in".into(),
                }),
                Err(error) => Err(error),
            },
            Some("ELIGIBLE") if claimable(&campaign) => {
                let (code, message) = self.claim(authorization).await?;
                if code != 0 {
                    return Ok(CheckinOutcome::Failed {
                        category: CheckinErrorCategory::Other,
                        summary: summary(&format!(
                            "CodeArts claim was rejected with code {code}: {message}"
                        )),
                    });
                }
                match self.confirm(authorization).await {
                    Ok(()) => Ok(CheckinOutcome::Succeeded {
                        summary: summary(if message.is_empty() {
                            "CodeArts daily check-in claimed"
                        } else {
                            &message
                        }),
                    }),
                    // Claim already succeeded; the points are credited. Confirm
                    // only changes the campaign status, so a transport failure
                    // leaves the day's outcome uncertain rather than false.
                    Err(error) => Err(error),
                }
            }
            Some(other) => Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: summary(&format!(
                    "CodeArts campaign status `{other}` is not claimable right now"
                )),
            }),
            None => Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "CodeArts campaign has no status".into(),
            }),
        }
    }
}

fn daily_campaign(payload: &Value) -> Option<&Value> {
    payload
        .get("data")?
        .get("items")?
        .as_array()?
        .iter()
        .find(|item| item.get("campaignId").and_then(Value::as_i64) == Some(DAILY_CAMPAIGN_ID))
}

fn status(campaign: &Value) -> Option<String> {
    campaign
        .get("status")
        .and_then(Value::as_str)
        .map(|status| status.trim().to_string())
        .filter(|status| !status.is_empty())
}

fn claimable(campaign: &Value) -> bool {
    campaign.get("claimable").and_then(Value::as_bool) == Some(true)
}

fn ended(campaign: &Value) -> bool {
    campaign
        .get("extra")
        .and_then(|extra| extra.get("endTime"))
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|end| end.with_timezone(&Utc) <= Utc::now())
        .unwrap_or(false)
}

fn business_code(payload: &Value) -> i64 {
    match payload.get("code") {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(-1),
        Some(Value::String(text)) => text.parse::<i64>().unwrap_or(-1),
        // Missing code means the upstream did not report a business result; the
        // call still reached the server, so this is not a success.
        _ => -1,
    }
}

fn business_message(payload: &Value) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

fn summary(text: &str) -> String {
    let redacted = crate::error::redact_secrets(text);
    redacted.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::NaiveDate;
    use serde_json::json;

    use super::*;
    use crate::account_service::{AccountService, CreateAccount};
    use crate::config::Region;
    use crate::security::SecretBox;
    use crate::storage::Storage;
    use crate::test_support::{spawn_server, Recorded, Reply};

    fn credential_json() -> Value {
        json!({
            "access_key_id": "AKID",
            "secret_access_key": "SECRET-KEY",
            "security_token": "STS-TOKEN",
            "domain_id": "domain-1",
        })
    }

    fn authorization() -> AuthorizationContext {
        AuthorizationContext {
            account_id: "account-1".into(),
            provider: "codearts-cn".into(),
            kind: ProviderKind::Codearts,
            region: Region::Cn,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(credential_json())),
            credential_revision: Some(1),
        }
    }

    fn request() -> CheckinRequest {
        CheckinRequest {
            account_id: "account-1".into(),
            kind: ProviderKind::Codearts,
            region: Region::Cn,
            business_date: NaiveDate::from_ymd_opt(2026, 10, 29).expect("date"),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn campaign(status: &str, claimable: bool) -> String {
        json!({
            "data": {"items": [
                {"campaignId": 3, "title": "推荐码", "status": "CONFIRMED"},
                {"campaignId": 1, "title": "每日签到", "status": status,
                 "claimable": claimable, "benefitAmount": 1000,
                 "extra": {"endTime": "2999-01-01T00:00:00Z"}}
            ]}
        })
        .to_string()
    }

    fn flow_handler(
        status: &'static str,
        claim_code: i64,
        fail_claim: bool,
    ) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
        move |request: &Recorded| match (request.method.as_str(), request.path()) {
            ("GET", "/v1/ops/delivery") => Reply::json(200, campaign(status, true)),
            ("POST", "/v1/ops/claim") if fail_claim => Reply::json(503, "maintenance"),
            ("POST", "/v1/ops/claim") => Reply::json(
                200,
                json!({"code": claim_code, "message": "ok"}).to_string(),
            ),
            ("POST", "/v1/ops/confirm") => Reply::json(200, json!({"code": 0}).to_string()),
            _ => Reply::json(404, "{}"),
        }
    }

    fn executor(server: &crate::test_support::Server) -> CodeArtsCheckinExecutor {
        CodeArtsCheckinExecutor::new(server.base_url.clone(), server.base_url.clone())
            .expect("executor")
    }

    #[tokio::test]
    async fn checkin_claims_then_confirms() {
        let server = spawn_server(flow_handler("ELIGIBLE", 0, false)).await;
        let outcome = executor(&server)
            .checkin(&request(), &authorization())
            .await
            .expect("checkin");
        assert_eq!(
            outcome,
            CheckinOutcome::Succeeded {
                summary: "ok".into()
            }
        );
        let paths: Vec<String> = server
            .requests()
            .iter()
            .map(|r| r.path().to_string())
            .collect();
        assert_eq!(
            paths,
            vec!["/v1/ops/delivery", "/v1/ops/claim", "/v1/ops/confirm"]
        );
        let claim = &server.requests()[1];
        let body: Value = serde_json::from_str(&claim.body).expect("claim body");
        assert_eq!(body["campaignId"], 1);
        assert_eq!(body["channel"], "IDE");
        assert!(claim
            .header("Authorization")
            .unwrap()
            .contains("Access=AKID"));
    }

    #[tokio::test]
    async fn already_confirmed_day_is_not_written_again() {
        let server = spawn_server(flow_handler("CONFIRMED", 0, false)).await;
        let outcome = executor(&server)
            .checkin(&request(), &authorization())
            .await
            .expect("checkin");
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
        assert_eq!(server.request_count(), 1);

        let probe = executor(&server)
            .check_status(&request(), &authorization())
            .await
            .expect("probe");
        assert_eq!(
            probe,
            CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
        );
    }

    #[tokio::test]
    async fn claimed_campaign_only_needs_confirm() {
        let server = spawn_server(flow_handler("CLAIMED", 0, false)).await;
        let outcome = executor(&server)
            .checkin(&request(), &authorization())
            .await
            .expect("checkin");
        assert!(matches!(outcome, CheckinOutcome::Succeeded { .. }));
        let paths: Vec<String> = server
            .requests()
            .iter()
            .map(|r| r.path().to_string())
            .collect();
        assert_eq!(paths, vec!["/v1/ops/delivery", "/v1/ops/confirm"]);
    }

    #[tokio::test]
    async fn claim_transport_failure_is_an_error_not_false_failure() {
        let server = spawn_server(flow_handler("ELIGIBLE", 0, true)).await;
        let error = executor(&server)
            .checkin(&request(), &authorization())
            .await
            .expect_err("transport failure");
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }

    #[tokio::test]
    async fn rejected_claim_is_a_definite_failure() {
        let server = spawn_server(flow_handler("ELIGIBLE", 40001, false)).await;
        let outcome = executor(&server)
            .checkin(&request(), &authorization())
            .await
            .expect("business failure");
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                ..
            }
        ));
        assert_eq!(server.request_count(), 2);
    }

    #[tokio::test]
    async fn successful_checkin_triggers_the_quota_hook() {
        use crate::checkin::{
            CheckinService, CheckinStatus, MockQuotaRefreshHook, RoutingCheckinExecutor,
        };

        let server = spawn_server(flow_handler("ELIGIBLE", 0, false)).await;
        let storage = Storage::open_memory().await.expect("db");
        let accounts = AccountService::new(
            storage.clone(),
            Arc::new(SecretBox::from_master_key(&[9u8; 32]).expect("key")),
        );
        let account = accounts
            .create(CreateAccount::new(
                ProviderKind::Codearts,
                Region::Cn,
                "alpha",
                Some(credential_json()),
            ))
            .await
            .expect("account");
        let routing = RoutingCheckinExecutor::new().with(
            ProviderKind::Codearts,
            Region::Cn,
            Arc::new(executor(&server)),
        );
        let quota = Arc::new(MockQuotaRefreshHook::new());
        let service = CheckinService::new(
            storage.clone(),
            accounts.clone(),
            Arc::new(routing),
            quota.clone(),
        );
        let result = service.checkin(&account.id).await.expect("checkin");
        assert_eq!(result.status, CheckinStatus::Succeeded);
        assert_eq!(quota.calls(), 1);
    }

    #[test]
    fn campaign_selection_uses_id_not_title() {
        let payload = json!({"data": {"items": [
            {"campaignId": 3, "title": "推荐码"},
            {"campaignId": 1, "title": "每日签到", "status": "ELIGIBLE", "claimable": true}
        ]}});
        let campaign = daily_campaign(&payload).expect("campaign");
        assert_eq!(status(campaign).as_deref(), Some("ELIGIBLE"));
        assert!(claimable(campaign));
    }

    #[test]
    fn missing_business_code_is_not_success() {
        assert_eq!(business_code(&json!({})), -1);
        assert_eq!(business_code(&json!({"code": 0})), 0);
        assert_eq!(business_code(&json!({"code": "40001"})), 40001);
    }

    #[test]
    fn ended_campaign_is_detected() {
        let campaign = json!({"extra": {"endTime": "2020-01-01T00:00:00Z"}});
        assert!(ended(&campaign));
        let campaign = json!({"extra": {"endTime": "2999-01-01T00:00:00Z"}});
        assert!(!ended(&campaign));
    }
}
