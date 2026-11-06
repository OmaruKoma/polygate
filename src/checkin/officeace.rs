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
        // remaining amount. A failed read is an unknown outcome: without a
        // baseline every remaining point would look like today's gain.
        let before_payload = self.http.get_subscription(&credential, now).await?;
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

        let payload = match self.http.claim_bonus(&credential, now).await {
            Ok(payload) => payload,
            Err(error) if ambiguous_claim_failure(&error) => {
                // The claim is idempotent for the day, but a blind retry could
                // double count. Ask first: no daily grant means nothing was
                // applied, so exactly one retry is safe.
                let probe = self.http.get_subscription(&credential, Utc::now()).await?;
                if subscription::daily_activity(&probe, Utc::now())
                    .is_some_and(|activity| activity.claimed)
                {
                    probe
                } else {
                    self.http.claim_bonus(&credential, Utc::now()).await?
                }
            }
            Err(error) => return Err(error),
        };
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

/// A claim whose answer was lost may or may not have been applied upstream.
fn ambiguous_claim_failure(error: &GatewayError) -> bool {
    matches!(
        error.kind,
        crate::error::ErrorKind::Timeout
            | crate::error::ErrorKind::Transport
            | crate::error::ErrorKind::UpstreamServer
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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

    /// A context whose gateway points at the mock server.
    fn context_at(base: &str) -> AuthorizationContext {
        let mut context = context();
        let mut value = context
            .credential
            .take()
            .expect("credential")
            .to_json()
            .clone();
        value["model_base_url"] = json!(base);
        context.credential = Some(Credential::from_value(value));
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
    async fn an_ambiguous_claim_that_did_not_land_is_retried_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let claims = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&claims);
        let before = subscription_with(0.0, "");
        let after = subscription_with(1000.0, "2026-10-30T01:00:00Z");
        let server = spawn_server(move |request| match request.path() {
            "/v1/subscription/bonus/claim" => {
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Reply::json(503, "{}")
                } else {
                    Reply::json(200, after.clone())
                }
            }
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

        let outcome = executor
            .checkin(&request(), &context_at(&server.base_url))
            .await
            .expect("checkin");
        assert!(
            matches!(outcome, CheckinOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        assert_eq!(claims.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_ambiguous_claim_that_landed_is_not_repeated() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let claims = Arc::new(AtomicUsize::new(0));
        let claim_counter = Arc::clone(&claims);
        let before = subscription_with(0.0, "");
        // "Landed" means the grant is effective today, so the probe sees it.
        let landed = subscription_with(1000.0, &chrono::Utc::now().to_rfc3339());
        let server = spawn_server(move |request| match request.path() {
            "/v1/subscription/bonus/claim" => {
                claim_counter.fetch_add(1, Ordering::SeqCst);
                Reply::json(503, "{}")
            }
            "/v1/subscription" => {
                // First read is the baseline; every later read sees the grant.
                let read = counter.fetch_add(1, Ordering::SeqCst);
                if read == 0 {
                    Reply::json(200, before.clone())
                } else {
                    Reply::json(200, landed.clone())
                }
            }
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = OfficeAceCheckinExecutor::new(
            "http://cloud.invalid",
            "http://cloud.invalid",
            true,
            reqwest::Client::new(),
        );

        let outcome = executor
            .checkin(&request(), &context_at(&server.base_url))
            .await
            .expect("checkin");
        assert!(
            matches!(
                outcome,
                CheckinOutcome::Succeeded { .. } | CheckinOutcome::AlreadyCheckedIn { .. }
            ),
            "{outcome:?}"
        );
        assert_eq!(claims.load(Ordering::SeqCst), 1, "no blind second claim");
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
    async fn a_failed_before_read_is_an_error_and_never_claims() {
        let server = spawn_server(|request| match request.path() {
            "/v1/subscription" => Reply::json(503, "unavailable"),
            "/v1/subscription/bonus/claim" => {
                Reply::json(200, subscription_with(100.0, "2026-10-30T01:00:00Z"))
            }
            _ => Reply::json(404, "{}"),
        })
        .await;
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

        let error = executor
            .checkin(&request(), &context)
            .await
            .expect_err("an unknown baseline must not be reported as a gain");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
        assert!(
            server
                .requests()
                .iter()
                .all(|request| request.path() != "/v1/subscription/bonus/claim"),
            "no claim without a baseline"
        );
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
