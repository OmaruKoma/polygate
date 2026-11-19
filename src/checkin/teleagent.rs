//! TeleAgent daily check-in executor (CN only).
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md` §7 and a live claim on 2026-11-19.
//! The desktop/web "积分福利社" card is driven by
//!
//! ```text
//! GET /superCowork/sapi/user/portal/companyStore/v3/checkDayTask
//!   → data.activity_data[] {action_status, action_type, jump_url}
//! GET /superCowork/sapi/user/portal/companyStore/checkTask?type=daily_login
//!   → the claim itself (the card flips to `completed`, balance +100)
//! GET /superCowork/sapi/user/portal/companyStore/checkTask
//!   → one-time tasks still claimable ({task_code, task_name, points_amount})
//! ```
//!
//! The check-in is a `GET` upstream; it is idempotent server-side: once the
//! card reports `completed`, a second call is not sent.
//!
//! Claims change account state, so they stay behind the provider option
//! `checkin.claim_enabled` like every other adapter in this repo.

use async_trait::async_trait;
use serde_json::Value;

use crate::account_service::AuthorizationContext;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use crate::providers::teleagent::credential::TeleagentCredential;
use crate::providers::teleagent::http::TeleagentHttp;
use crate::providers::teleagent::region;

/// `action_status` values seen on the welfare card.
const STATUS_COMPLETED: &str = "completed";
const STATUS_INCOMPLETE: &str = "incomplete";

pub struct TeleagentCheckinExecutor {
    http: TeleagentHttp,
    claim_enabled: bool,
}

impl TeleagentCheckinExecutor {
    pub fn new(claim_enabled: bool, client: reqwest::Client) -> Self {
        Self {
            http: TeleagentHttp::new(client),
            claim_enabled,
        }
    }

    /// Tests only.
    pub fn with_base(
        claim_enabled: bool,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self {
            http: TeleagentHttp::with_base(client, base),
            claim_enabled,
        }
    }

    fn credential(
        &self,
        authorization: &AuthorizationContext,
    ) -> Result<TeleagentCredential, GatewayError> {
        if authorization.region != Region::Cn {
            return Err(GatewayError::unsupported(
                "TeleAgent check-in is not available for this account's region",
            ));
        }
        let credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("TeleAgent account has no credential"))?;
        TeleagentCredential::from_credential(credential)
    }

    async fn daily_status(
        &self,
        credential: &TeleagentCredential,
    ) -> Result<DailyStatus, GatewayError> {
        let payload = self
            .http
            .get_portal_json(credential, region::DAY_TASK_PATH)
            .await?;
        parse_daily(&payload)
    }

    /// One-time tasks that are still claimable. Claiming them is best-effort:
    /// the claim URL shape is `checkTask?type=<task_code>`; any rejection is
    /// reported, never retried blindly.
    async fn claim_one_time(
        &self,
        credential: &TeleagentCredential,
    ) -> Result<Vec<String>, GatewayError> {
        let payload = self
            .http
            .get_portal_json(credential, region::CLAIMABLE_TASKS_PATH)
            .await?;
        let items = payload
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut claimed = Vec::new();
        for item in items {
            let Some(code) = item.get("task_code").and_then(Value::as_str) else {
                continue;
            };
            if !code
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            {
                continue;
            }
            let path = format!("{}?type={code}", region::CLAIMABLE_TASKS_PATH);
            match self.http.get_portal_json(credential, &path).await {
                Ok(payload) if payload.get("code").and_then(Value::as_i64) == Some(200) => {
                    let amount = item
                        .get("points_amount")
                        .and_then(Value::as_f64)
                        .map(|value| format!("（+{value}）"))
                        .unwrap_or_default();
                    claimed.push(format!("{code}{amount}"));
                }
                Ok(_) => {}
                Err(error) if matches!(error.kind, crate::error::ErrorKind::Transport) => {
                    return Err(error)
                }
                Err(_) => {}
            }
        }
        Ok(claimed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DailyStatus {
    /// `completed` / `incomplete`; `None` when the card is missing.
    state: Option<String>,
}

fn parse_daily(payload: &Value) -> Result<DailyStatus, GatewayError> {
    if payload.get("code").and_then(Value::as_i64) != Some(200) {
        return Err(GatewayError::upstream_server(
            "TeleAgent welfare page returned a non-success envelope",
        ));
    }
    let activities = payload
        .pointer("/data/activity_data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GatewayError::upstream_server("TeleAgent welfare page has no activity_data array")
        })?;
    for activity in activities {
        let url = activity
            .get("jump_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if url.contains("checkTask?type=") {
            return Ok(DailyStatus {
                state: activity
                    .get("action_status")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    Ok(DailyStatus { state: None })
}

fn is_completed(status: &DailyStatus) -> bool {
    status
        .state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case(STATUS_COMPLETED))
}

fn is_incomplete(status: &DailyStatus) -> bool {
    status
        .state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case(STATUS_INCOMPLETE))
}

#[async_trait]
impl CheckinExecutor for TeleagentCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Teleagent
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        let credential = self.credential(authorization)?;
        let status = self.daily_status(&credential).await?;
        if is_completed(&status) {
            return Ok(CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn));
        }
        Ok(CheckinProbe::Unknown)
    }

    async fn checkin(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let credential = self.credential(authorization)?;
        let status = self.daily_status(&credential).await?;
        if is_completed(&status) {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "TeleAgent 福利社显示今天已经领过".into(),
            });
        }
        if !is_incomplete(&status) {
            return Err(GatewayError::unsupported(
                "TeleAgent 福利社没有今天的签到卡片（活动可能已结束）",
            ));
        }
        if !self.claim_enabled {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "TeleAgent 积分领取默认关闭；设置 provider 选项 \
                          `checkin.claim_enabled = true` 后才会领取"
                    .into(),
            });
        }

        let balance_before = self.balance(&credential).await;
        let payload = self
            .http
            .get_portal_json(&credential, region::DAILY_CLAIM_PATH)
            .await?;
        if payload.get("code").and_then(Value::as_i64) != Some(200) {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: format!(
                    "TeleAgent 拒绝领取：{}",
                    payload
                        .get("msg")
                        .and_then(Value::as_str)
                        .unwrap_or("上游返回非 200 code")
                ),
            });
        }

        let after = self.daily_status(&credential).await?;
        if !is_completed(&after) {
            // The claim answered 200 but the card did not flip; report the
            // upstream answer rather than claiming success.
            return Ok(CheckinOutcome::Unknown {
                summary: "TeleAgent 领取请求已发送，但福利社卡片没有变成 completed".into(),
            });
        }

        let one_time = self.claim_one_time(&credential).await.unwrap_or_default();
        let balance_after = self.balance(&credential).await;
        let delta = match (balance_before, balance_after) {
            (Some(before), Some(after)) => Some(after - before),
            (None, Some(after)) => Some(after),
            _ => None,
        };
        let mut summary = match delta {
            Some(delta) if delta > 0.0 => format!("TeleAgent 签到成功，积分 +{delta:.2}"),
            _ => "TeleAgent 签到成功".to_string(),
        };
        if !one_time.is_empty() {
            summary.push_str(&format!("；一次性礼包：{}", one_time.join(", ")));
        }
        Ok(CheckinOutcome::Succeeded { summary })
    }
}

impl TeleagentCheckinExecutor {
    async fn balance(&self, credential: &TeleagentCredential) -> Option<f64> {
        let payload = self.http.get_user_center(credential).await.ok()?;
        let raw = payload.pointer("/data/current_points_balance")?;
        match raw {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse::<f64>().ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;
    use crate::test_support::{spawn_server, Reply};
    use serde_json::json;

    fn authorization() -> AuthorizationContext {
        AuthorizationContext {
            account_id: "acc-1".into(),
            provider: "teleagent-cn".into(),
            kind: ProviderKind::Teleagent,
            region: Region::Cn,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(json!({
                "token": "Em9xR4:header.payload.signature-part",
                "device_id": "dev-1",
                "install_id": "install-1",
            }))),
            credential_revision: Some(0),
        }
    }

    fn request() -> CheckinRequest {
        CheckinRequest {
            account_id: "acc-1".into(),
            kind: ProviderKind::Teleagent,
            region: Region::Cn,
            business_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn day_task(status: &str) -> String {
        json!({
            "code": 200, "msg": "success",
            "data": {"brand_title": "积分福利社", "activity_data": [
                {"activity_title": "双11", "action_type": "web", "action_status": "fixed",
                 "jump_url": "https://agent.teleai.com.cn/meter/teleagent/user/portal/campaign"},
                {"activity_title": "每日可领100积分", "action_type": "api",
                 "action_status": status,
                 "jump_url": "/superCowork/sapi/user/portal/companyStore/checkTask?type=daily_login"}
            ]}
        })
        .to_string()
    }

    #[tokio::test]
    async fn a_completed_card_probes_as_already_checked_in() {
        let server = spawn_server(|_| Reply::json(200, day_task("completed"))).await;
        let executor =
            TeleagentCheckinExecutor::with_base(false, reqwest::Client::new(), &server.base_url);
        let probe = executor
            .check_status(&request(), &authorization())
            .await
            .expect("probe");
        assert_eq!(
            probe,
            CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
        );
    }

    #[tokio::test]
    async fn an_incomplete_card_is_claimed_and_confirmed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let server = spawn_server(move |request| {
            // `path()` drops the query; the claim is `checkTask?type=…`, so
            // match the full target for it.
            let target = request.target.as_str();
            if target == region::DAILY_CLAIM_PATH {
                counter.fetch_add(1, Ordering::SeqCst);
                return Reply::json(
                    200,
                    json!({"code": 200, "msg": "success", "data": []}).to_string(),
                );
            }
            match request.path() {
                region::DAY_TASK_PATH => {
                    let n = counter.load(Ordering::SeqCst);
                    let status = if n == 0 { "incomplete" } else { "completed" };
                    Reply::json(200, day_task(status))
                }
                region::CLAIMABLE_TASKS_PATH => Reply::json(
                    200,
                    json!({"code": 200, "msg": "success", "data": []}).to_string(),
                ),
                region::USER_CENTER_PATH => Reply::json(
                    200,
                    json!({"code": 200, "data": {"current_points_balance": "9196.15"}}).to_string(),
                ),
                _ => Reply::json(404, "{}"),
            }
        })
        .await;
        let executor =
            TeleagentCheckinExecutor::with_base(true, reqwest::Client::new(), &server.base_url);
        let outcome = executor
            .checkin(&request(), &authorization())
            .await
            .expect("checkin");
        match outcome {
            CheckinOutcome::Succeeded { summary } => {
                assert!(summary.contains("签到成功"), "{summary}");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_disabled_claim_reports_the_switch_instead_of_claiming() {
        let server = spawn_server(|_| Reply::json(200, day_task("incomplete"))).await;
        let executor =
            TeleagentCheckinExecutor::with_base(false, reqwest::Client::new(), &server.base_url);
        let outcome = executor
            .checkin(&request(), &authorization())
            .await
            .expect("checkin");
        assert!(matches!(outcome, CheckinOutcome::Failed { .. }));
        // Only the status probe ran; no claim path was touched.
        assert!(server
            .requests()
            .iter()
            .all(|recorded| recorded.target == region::DAY_TASK_PATH));
    }
}
