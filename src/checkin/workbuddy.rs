//! WorkBuddy daily check-in executor (CN only).
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §11.2 and §13; Coding2API
//! `src/provider/codebuddy/checkin.py` (MIT, protocol facts only) plus the
//! operator's `workbuddy2api/admin/backend.py:342-367`.
//!
//! ```text
//! POST {billing_base}/v2/billing/meter/checkin-activity-status  body {}
//! POST {billing_base}/v2/billing/meter/daily-checkin            body {}
//! ```
//!
//! Status: `code == 0` and `data.today_checked_in`; `active` tells whether the
//! campaign is running at all.
//!
//! Claim: `code == 0` is success. "Already claimed" arrives as `code 1001`
//! (workbuddy2api) or `code 10001` and/or `msg` containing 已签到 (Coding2API,
//! often with HTTP 400); both are idempotent and reported as
//! `AlreadyCheckedIn`. `1002`/`1003` are definitive rejections. An unknown
//! business code is also a definitive rejection (the upstream answered), while
//! transport/5xx failures stay errors so the service records `Unknown`.
//!
//! Intl is marked `Unsupported`: Coding2API disables check-in for the
//! international realm and no live evidence contradicts that. The executor
//! never sends a CN endpoint with an Intl credential.
//!
//! Claims change account state, so they only run when `checkin.claim_enabled`
//! is set for the provider.

use async_trait::async_trait;
use serde_json::Value;

use crate::account_service::AuthorizationContext;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use crate::providers::workbuddy::credential::WorkbuddyCredential;
use crate::providers::workbuddy::error::{classify_http_status, classify_payload};
use crate::providers::workbuddy::http::WorkbuddyHttp;
use crate::providers::workbuddy::region;

const CODE_ALREADY_1001: i64 = 1001;
const CODE_NOT_ELIGIBLE: i64 = 1002;
const CODE_EVENT_ENDED: i64 = 1003;
const CODE_ALREADY_10001: i64 = 10001;
const ALREADY_MARKER: &str = "已签到";

pub struct WorkbuddyCheckinExecutor {
    http: WorkbuddyHttp,
    region: Region,
    claim_enabled: bool,
}

impl WorkbuddyCheckinExecutor {
    pub fn new(region: Region, claim_enabled: bool, client: reqwest::Client) -> Self {
        Self {
            http: WorkbuddyHttp::new(client, region),
            region,
            claim_enabled,
        }
    }

    /// Tests only.
    pub fn with_base(
        region: Region,
        claim_enabled: bool,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self {
            http: WorkbuddyHttp::with_base(client, region, base),
            region,
            claim_enabled,
        }
    }

    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        self.http.set_identity(identity);
        self
    }

    pub fn claim_enabled(&self) -> bool {
        self.claim_enabled
    }

    fn credential(
        &self,
        authorization: &AuthorizationContext,
    ) -> Result<WorkbuddyCredential, GatewayError> {
        if self.region != authorization.region {
            // A CN executor must never talk to an Intl account (or vice versa).
            return Err(GatewayError::unsupported(
                "WorkBuddy check-in is not available for this account's region",
            ));
        }
        let credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("WorkBuddy account has no credential"))?;
        WorkbuddyCredential::from_credential(credential, authorization.region)
    }

    fn empty() -> Value {
        Value::Object(Default::default())
    }

    async fn status(&self, credential: &WorkbuddyCredential) -> Result<StatusView, GatewayError> {
        let payload = self
            .http
            .post_billing_json(region::CHECKIN_STATUS_PATH, &Self::empty(), credential)
            .await?;
        parse_status(&payload)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusView {
    active: bool,
    today_checked_in: bool,
}

fn parse_status(payload: &Value) -> Result<StatusView, GatewayError> {
    if let Some(error) = classify_payload(payload) {
        return Err(error);
    }
    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            GatewayError::upstream_server("WorkBuddy check-in status has no data object")
        })?;
    Ok(StatusView {
        active: flag(data.get("active")),
        today_checked_in: flag(data.get("today_checked_in")) || flag(data.get("todayCheckedIn")),
    })
}

fn flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_i64().is_some_and(|value| value != 0),
        _ => false,
    }
}

fn business_code(payload: &Value) -> Option<i64> {
    match payload.get("code")? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn message_of(payload: &Value) -> String {
    payload
        .get("msg")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn credit_of(payload: &Value) -> Option<f64> {
    let candidate = payload
        .get("data")
        .and_then(|data| data.get("credit"))
        .or_else(|| payload.get("credit"))?;
    if candidate.is_boolean() {
        return None;
    }
    candidate.as_f64().filter(|value| value.is_finite())
}

#[async_trait]
impl CheckinExecutor for WorkbuddyCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Workbuddy
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        if self.region == Region::Intl {
            return Err(GatewayError::unsupported(
                "WorkBuddy check-in is not supported on the international realm",
            ));
        }
        let credential = self.credential(authorization)?;
        let status = self.status(&credential).await?;
        if !status.active {
            return Err(GatewayError::unsupported(
                "WorkBuddy reports no active check-in programme for this account",
            ));
        }
        if status.today_checked_in {
            return Ok(CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn));
        }
        Ok(CheckinProbe::Unknown)
    }

    async fn checkin(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        if self.region == Region::Intl {
            return Ok(CheckinOutcome::Unsupported);
        }
        let credential = self.credential(authorization)?;
        let status = self.status(&credential).await?;
        if !status.active {
            return Ok(CheckinOutcome::Unsupported);
        }
        if status.today_checked_in {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "WorkBuddy reports today as already checked in".into(),
            });
        }
        if !self.claim_enabled {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "WorkBuddy reward claim is disabled; set the provider option \
                          `checkin.claim_enabled = true` to allow claims"
                    .into(),
            });
        }

        let (http_status, body) = self
            .http
            .post_billing_raw(region::CHECKIN_CLAIM_PATH, &Self::empty(), &credential)
            .await?;
        if http_status == 401 || http_status == 403 {
            return Err(classify_http_status(http_status, &body));
        }
        if http_status >= 500 {
            // The claim may have been applied; the service records `Unknown`.
            return Err(classify_http_status(http_status, &body));
        }
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("WorkBuddy check-in returned unparsable JSON")
                .with_upstream_status(http_status)
        })?;
        let code = business_code(&payload);
        let message = message_of(&payload);
        if code == Some(CODE_ALREADY_1001)
            || code == Some(CODE_ALREADY_10001)
            || message.contains(ALREADY_MARKER)
        {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "WorkBuddy reports today's reward as already claimed".into(),
            });
        }
        if code == Some(CODE_NOT_ELIGIBLE) || code == Some(CODE_EVENT_ENDED) {
            let label = if code == Some(CODE_NOT_ELIGIBLE) {
                "this account is not eligible"
            } else {
                "the check-in programme has ended"
            };
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: format!("WorkBuddy declined the claim: {label}"),
            });
        }
        if code != Some(0) {
            if http_status >= 400 {
                return Err(classify_http_status(http_status, &body));
            }
            let label = code.map_or_else(
                || "without a business code".to_string(),
                |code| format!("with code {code}"),
            );
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: format!("WorkBuddy declined the claim {label}"),
            });
        }
        Ok(CheckinOutcome::Succeeded {
            summary: match credit_of(&payload) {
                Some(credit) => format!("checked in; WorkBuddy granted {credit} credits"),
                None => "checked in; WorkBuddy did not report the reward amount".into(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;
    use crate::test_support::{spawn_server, Reply};
    use serde_json::json;

    fn context(region: Region) -> AuthorizationContext {
        AuthorizationContext {
            account_id: "acct-1".into(),
            provider: format!("workbuddy-{}", region.as_str()),
            kind: ProviderKind::Workbuddy,
            region,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(json!({
                "access_token": "jwt-access",
                "uid": "uid-1",
                "domain": "www.codebuddy.cn",
            }))),
            credential_revision: Some(0),
        }
    }

    fn request(region: Region) -> CheckinRequest {
        CheckinRequest {
            account_id: "acct-1".into(),
            kind: ProviderKind::Workbuddy,
            region,
            business_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn status_body(active: bool, checked: bool) -> String {
        json!({"code": 0, "data": {"active": active, "today_checked_in": checked}}).to_string()
    }

    #[tokio::test]
    async fn claim_succeeds_and_reports_credit() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("POST", "/v2/billing/meter/checkin-activity-status") => {
                Reply::json(200, status_body(true, false))
            }
            ("POST", "/v2/billing/meter/daily-checkin") => {
                Reply::json(200, json!({"code": 0, "data": {"credit": 100}}).to_string())
            }
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        let CheckinOutcome::Succeeded { summary } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert!(summary.contains("100"), "{summary}");
        let claim = server
            .requests()
            .into_iter()
            .find(|request| request.path() == "/v2/billing/meter/daily-checkin")
            .expect("claim posted");
        assert_eq!(claim.header("authorization"), Some("Bearer jwt-access"));
    }

    #[tokio::test]
    async fn a_disabled_claim_never_posts() {
        let server = spawn_server(|request| match request.path() {
            "/v2/billing/meter/checkin-activity-status" => {
                Reply::json(200, status_body(true, false))
            }
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Cn,
            false,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                ..
            }
        ));
        assert!(server
            .requests()
            .iter()
            .all(|request| request.path() != "/v2/billing/meter/daily-checkin"));
    }

    #[tokio::test]
    async fn already_claimed_codes_are_not_failures() {
        for body in [
            json!({"code": 1001, "msg": "already claimed"}),
            json!({"code": 10001, "msg": "已签到"}),
            json!({"code": 500, "msg": "今天已签到"}),
        ] {
            let body = body.to_string();
            let reply = body.clone();
            let server = spawn_server(move |request| match request.path() {
                "/v2/billing/meter/checkin-activity-status" => {
                    Reply::json(200, status_body(true, false))
                }
                "/v2/billing/meter/daily-checkin" => Reply::json(400, reply.clone()),
                _ => Reply::json(404, "{}"),
            })
            .await;
            let executor = WorkbuddyCheckinExecutor::with_base(
                Region::Cn,
                true,
                reqwest::Client::new(),
                &server.base_url,
            );
            let outcome = executor
                .checkin(&request(Region::Cn), &context(Region::Cn))
                .await
                .unwrap();
            assert!(
                matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }),
                "{body} -> {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn ineligible_and_ended_are_definitive_failures() {
        for (code, marker) in [(1002, "eligible"), (1003, "ended")] {
            let body = json!({"code": code, "msg": "no"}).to_string();
            let reply = body.clone();
            let server = spawn_server(move |request| match request.path() {
                "/v2/billing/meter/checkin-activity-status" => {
                    Reply::json(200, status_body(true, false))
                }
                "/v2/billing/meter/daily-checkin" => Reply::json(200, reply.clone()),
                _ => Reply::json(404, "{}"),
            })
            .await;
            let executor = WorkbuddyCheckinExecutor::with_base(
                Region::Cn,
                true,
                reqwest::Client::new(),
                &server.base_url,
            );
            let outcome = executor
                .checkin(&request(Region::Cn), &context(Region::Cn))
                .await
                .unwrap();
            let CheckinOutcome::Failed { summary, .. } = outcome else {
                panic!("expected failure for {code}, got {outcome:?}");
            };
            assert!(summary.contains(marker), "{summary}");
        }
    }

    #[tokio::test]
    async fn an_already_checked_in_status_skips_the_claim() {
        let server = spawn_server(|request| match request.path() {
            "/v2/billing/meter/checkin-activity-status" => {
                Reply::json(200, status_body(true, true))
            }
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
        assert_eq!(server.request_count(), 1);

        let probe = executor
            .check_status(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert_eq!(
            probe,
            CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
        );
    }

    #[tokio::test]
    async fn an_inactive_programme_is_unsupported() {
        let server = spawn_server(|_| Reply::json(200, status_body(false, false))).await;
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        assert!(matches!(
            executor
                .checkin(&request(Region::Cn), &context(Region::Cn))
                .await
                .unwrap(),
            CheckinOutcome::Unsupported
        ));
        assert_eq!(
            executor
                .check_status(&request(Region::Cn), &context(Region::Cn))
                .await
                .unwrap_err()
                .kind,
            crate::error::ErrorKind::Unsupported
        );
    }

    #[tokio::test]
    async fn intl_is_unsupported_without_a_request() {
        let server = spawn_server(|_| Reply::json(200, status_body(true, false))).await;
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        assert!(matches!(
            executor
                .checkin(&request(Region::Intl), &context(Region::Intl))
                .await
                .unwrap(),
            CheckinOutcome::Unsupported
        ));
        assert!(executor
            .check_status(&request(Region::Intl), &context(Region::Intl))
            .await
            .is_err());
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn a_transport_failure_stays_an_error() {
        // Point at a closed port: the service maps this to `Unknown`, never
        // `Failed`, because the claim may have been applied.
        let executor = WorkbuddyCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            "http://127.0.0.1:9",
        );
        assert!(executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .is_err());
    }
}
