//! OfficeAce daily bonus executor (`/v1/subscription/bonus/claim`).
//!
//! Evidence: Coding2API `src/provider/officeace/client.py` (`claim_bonus`,
//! `fetch_checkin_status`) and `events.py` (`summarize_subscription`). Points
//! are the main ledger: a new account gets a one-off grant and the daily
//! activity is worth `1000` points in the reference measurement, each grant
//! expiring on its own schedule.
//!
//! Facts this adapter depends on:
//!
//! * the claim is a **write** and is idempotent per day upstream, so a repeated
//!   claim answers 200 with unchanged `bonus_skus`;
//! * the body must be an empty JSON object: a bare POST answers 500
//!   `OfficeAce.11020001`;
//! * the gain is the **remaining** delta of `bonus_skus`, not `points`
//!   (that field accumulates grants from every client);
//! * quota and check-in need the temporary AK/SK. A chat-only credential is
//!   reported as unsupported with the reason, never as a claim attempt.
//!
//! Claim safety: reward claims change account state, so they only run when
//! `checkin.claim_enabled` is set for the provider.

use async_trait::async_trait;
use chrono::Utc;

use crate::account_service::AuthorizationContext;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::ProviderKind;
use crate::error::GatewayError;
use crate::providers::officeace::credential::OfficeAceCredential;
use crate::providers::officeace::http::OfficeAceHttp;
use crate::providers::officeace::subscription;

pub struct OfficeAceCheckinExecutor {
    http: OfficeAceHttp,
    claim_enabled: bool,
}

impl OfficeAceCheckinExecutor {
    pub fn new(
        cloud: impl Into<String>,
        sts: impl Into<String>,
        claim_enabled: bool,
        client: reqwest::Client,
    ) -> Self {
        Self {
            http: OfficeAceHttp::with_endpoints(client, cloud, sts),
            claim_enabled,
        }
    }

    pub fn claim_enabled(&self) -> bool {
        self.claim_enabled
    }

    fn credential(
        authorization: &AuthorizationContext,
    ) -> Result<OfficeAceCredential, GatewayError> {
        let credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("OfficeAce account has no credential"))?;
        OfficeAceCredential::from_credential(credential)
    }

    fn require_control_plane(credential: &OfficeAceCredential) -> Result<(), GatewayError> {
        if credential.has_control_plane() {
            Ok(())
        } else {
            Err(GatewayError::unsupported(
                "OfficeAce account has only a chat credential (no temporary AK/SK); \
                 re-authorize it to enable points and check-in",
            ))
        }
    }
}

#[async_trait]
impl CheckinExecutor for OfficeAceCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Officeace
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        let credential = Self::credential(authorization)?;
        Self::require_control_plane(&credential)?;
        let payload = self.http.get_subscription(&credential, Utc::now()).await?;
        // The daily entry only exists once it has been granted; a missing entry
        // means "not claimed today", not "this provider has no check-in".
        match subscription::daily_activity(&payload, Utc::now()) {
            Some(activity) if activity.claimed => {
                Ok(CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn))
            }
            _ => Ok(CheckinProbe::Unknown),
        }
    }

    async fn checkin(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let credential = Self::credential(authorization)?;
        Self::require_control_plane(&credential)?;
        let now = Utc::now();

        // One read serves both the "already claimed" check and the before
        // snapshot; `points` is cumulative, so the gain is measured on the
        // remaining amount.
        let before_payload = self
            .http
            .get_subscription(&credential, now)
            .await
            .unwrap_or(serde_json::Value::Null);
        if let Some(activity) =
            subscription::daily_activity(&before_payload, now).filter(|activity| activity.claimed)
        {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: format!(
                    "OfficeAce reports today's {} as already claimed",
                    activity.activity_name
                ),
            });
        }
        let before = subscription::bonus_remaining(&before_payload);
        if !self.claim_enabled {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "OfficeAce reward claim is disabled; set the provider option \
                          `checkin.claim_enabled = true` to allow claims"
                    .into(),
            });
        }

        let payload = self.http.claim_bonus(&credential, now).await?;
        let after = subscription::bonus_remaining(&payload);
        let gained: f64 = after
            .iter()
            .map(|(activity, remaining)| {
                (remaining - before.get(activity).copied().unwrap_or(0.0)).max(0.0)
            })
            .sum();
        if gained > 0.0 {
            return Ok(CheckinOutcome::Succeeded {
                summary: format!("checked in; OfficeAce granted {gained} points"),
            });
        }
        if !before.is_empty() && after.keys().all(|key| after.get(key) <= before.get(key)) {
            // The upstream is idempotent per day: an unchanged snapshot means
            // the reward was already collected (possibly from the desktop app).
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "OfficeAce reports today's bonus as already claimed".into(),
            });
        }
        Ok(CheckinOutcome::Failed {
            category: CheckinErrorCategory::Upstream,
            summary: "OfficeAce claim response carried no bonus activity; retry later".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;
    use crate::config::Region;
    use crate::error::ErrorKind;
    use crate::test_support::{spawn_server, Reply};
    use serde_json::json;

    fn context() -> AuthorizationContext {
        AuthorizationContext {
            account_id: "acct-1".into(),
            provider: "officeace-cn".into(),
            kind: ProviderKind::Officeace,
            region: Region::Cn,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(json!({
                "model_app_key": "app",
                "model_app_secret": "secret",
                "model_base_url": "http://gateway.invalid",
                "access_key_id": "sts-ak",
                "secret_access_key": "sts-sk",
                "security_token": "sts-token",
                "project_id": "project-1",
                "expiration": "2030-01-01T00:00:00Z",
            }))),
            credential_revision: Some(0),
        }
    }

    fn chat_only_context() -> AuthorizationContext {
        let mut context = context();
        context.credential = Some(Credential::from_value(json!({
            "model_app_key": "app",
            "model_app_secret": "secret",
            "model_base_url": "http://gateway.invalid",
        })));
        context
    }

    fn request() -> CheckinRequest {
        CheckinRequest {
            account_id: "acct-1".into(),
            kind: ProviderKind::Officeace,
            region: Region::Cn,
            business_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn subscription_with(remaining: f64, effective: &str) -> String {
        json!({
            "skus": [{"quotas": [{
                "sku_attr_code": "officeace_points", "sku_value": "4000",
                "current_value": "0", "expired_time": "2030-01-01T00:00:00Z"
            }]}],
            "bonus_skus": [{
                "activity_id": "bonus-daily", "activity_name": "每日签到",
                "cbc_resource_id": "bonus:daily", "points": "1000",
                "current_value": (1000.0 - remaining).to_string(),
                "effective_time": effective,
                "expired_time": "2030-01-01T00:00:00Z"
            }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn a_claim_that_adds_points_succeeds_and_reports_the_gain() {
        let before = subscription_with(0.0, "");
        let after = subscription_with(1000.0, "2026-10-30T01:00:00Z");
        let server = spawn_server(move |request| match request.path() {
            "/v1/subscription/bonus/claim" => Reply::json(200, after.clone()),
            "/v1/subscription" => Reply::json(200, before.clone()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            true,
            reqwest::Client::new(),
        );
        // The gateway address comes from the credential; point it at the mock.
        let mut context = context();
        let mut value = context
            .credential
            .take()
            .expect("credential")
            .to_json()
            .clone();
        value["model_base_url"] = json!(server.base_url);
        context.credential = Some(Credential::from_value(value));

        let outcome = executor
            .checkin(&request(), &context)
            .await
            .expect("checkin");
        match outcome {
            CheckinOutcome::Succeeded { summary } => {
                assert!(summary.contains("1000"), "summary: {summary}");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        let claim = server
            .requests()
            .into_iter()
            .find(|request| request.path() == "/v1/subscription/bonus/claim")
            .expect("claim request");
        assert_eq!(claim.method, "POST");
        assert_eq!(claim.body, "{}");
        assert!(claim
            .header("authorization")
            .unwrap_or_default()
            .starts_with("V11-HMAC-SHA256 Credential=sts-ak/"));
        assert_eq!(claim.header("x-subscription-type"), Some("v2"));
    }

    #[tokio::test]
    async fn a_claim_disabled_provider_never_writes() {
        let server = spawn_server(|_| Reply::json(200, subscription_with(0.0, ""))).await;
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            false,
            reqwest::Client::new(),
        );
        let mut context = context();
        let mut value = context
            .credential
            .take()
            .expect("credential")
            .to_json()
            .clone();
        value["model_base_url"] = json!(server.base_url);
        context.credential = Some(Credential::from_value(value));

        let outcome = executor
            .checkin(&request(), &context)
            .await
            .expect("checkin");
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                ..
            }
        ));
        assert!(output(&outcome).contains("checkin.claim_enabled"));
        assert!(server
            .requests()
            .iter()
            .all(|request| request.path() == "/v1/subscription"));
    }

    #[tokio::test]
    async fn an_unchanged_snapshot_means_already_claimed() {
        let same = subscription_with(0.0, "");
        let server = spawn_server(move |_| Reply::json(200, same.clone())).await;
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            true,
            reqwest::Client::new(),
        );
        let mut context = context();
        let mut value = context
            .credential
            .take()
            .expect("credential")
            .to_json()
            .clone();
        value["model_base_url"] = json!(server.base_url);
        context.credential = Some(Credential::from_value(value));

        let outcome = executor
            .checkin(&request(), &context)
            .await
            .expect("checkin");
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
    }

    #[tokio::test]
    async fn a_chat_only_credential_is_unsupported_with_a_reason() {
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            true,
            reqwest::Client::new(),
        );
        let error = executor
            .checkin(&request(), &chat_only_context())
            .await
            .expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("re-authorize"));
    }

    #[tokio::test]
    async fn a_claimed_daily_entry_is_reported_without_a_write() {
        let claimed = subscription_with(1000.0, &Utc::now().to_rfc3339());
        let server = spawn_server(move |_| Reply::json(200, claimed.clone())).await;
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            true,
            reqwest::Client::new(),
        );
        let mut context = context();
        let mut value = context
            .credential
            .take()
            .expect("credential")
            .to_json()
            .clone();
        value["model_base_url"] = json!(server.base_url);
        context.credential = Some(Credential::from_value(value));

        let probe = executor
            .check_status(&request(), &context)
            .await
            .expect("status");
        assert_eq!(
            probe,
            CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
        );
        let outcome = executor
            .checkin(&request(), &context)
            .await
            .expect("checkin");
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
        assert!(server
            .requests()
            .iter()
            .all(|request| request.path() != "/v1/subscription/bonus/claim"));
    }

    fn output(outcome: &CheckinOutcome) -> &str {
        outcome.summary()
    }
}
