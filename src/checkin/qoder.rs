//! Qoder daily check-in executor (campaign API).
//!
//! Evidence: Coding2API `src/provider/qoder/client.py:444-530` and
//! `events.py:564-720`; observed on a real CN account (reference author) and
//! summarized in `docs/QODER_PROVIDER_RESEARCH.md` §10.
//!
//! Facts this adapter depends on:
//! * `GET /sash/api/v1/me/campaigns` returns `{"campaigns": [...]}` and **must**
//!   carry `Cosy-ClientType: 10`; without it the upstream silently answers with an
//!   empty list instead of an error.
//! * Only `actionType == "CLAIM_BENEFIT"` entries are claimable; display slots
//!   (`VIEW_DETAILS`) are also `CLAIMABLE` and must be filtered out.
//! * `CLAIMED` means today is done; `BLOCKED` +
//!   `SAME_PERSON_ALREADY_CLAIMED` is mapped to "already checked in", because the
//!   same natural person claimed from another account. Any other `BLOCKED` is a
//!   real failure.
//! * `replayed: true` in the claim response is an idempotent replay.
//! * The campaign list is delivered per account. An account without the daily
//!   check-in keeps the global `VIEW_DETAILS` promo, so "no check-in" shows up
//!   as a non-empty list without any `CLAIM_BENEFIT` entry, not as an empty
//!   list (capture `Untitledtest1.trace`, Intl, 2026-11-01).
//! * `showCampaign` is a growth-page feature flag for the request (it turns
//!   false when `cosy-clienttype` is missing), **not** an activity flag: the
//!   account above still reports `showCampaign: true`. An empty list with the
//!   flag on is therefore an upstream degradation, not a settled outcome.
//! * 404/405/410 on the campaign API means "this region has no campaign
//!   endpoint". That is reported as `Unsupported` (settled, never retried), not
//!   as a failure.
//! * `showCampaign` separates a genuine empty list (the account is not targeted,
//!   `false`) from the upstream's silent degradation (`true` with `campaigns:
//!   []`), which is reported as a retryable upstream failure instead of a
//!   settled "unsupported".
//!
//! Intl capture (`qoder-intel.trace`, desktop 0.5.1, 2026-11-01 11:38 +08): the
//! same `/sash/api/v1/me/campaigns` API on `openapi.qoder.sh`; the daily
//! campaign is `CLAIM_BENEFIT` + 100 credits, resets at 10:00 UTC+8 and stays
//! valid 30 days after claiming. `cosy-clienttype: 10` is still mandatory —
//! without it the upstream answers 200 with `showCampaign: false` and an empty
//! list. The claim POST carries no body in the desktop client; `{}` is accepted
//! as well (verified 2026-11-01, answered `replayed: true`).
//!
//! Claim safety: reward claims change account state. They only run when
//! `claim_enabled` is set for the provider; otherwise a run reports a definitive
//! failure that names the setting instead of silently claiming.

use async_trait::async_trait;
use serde_json::Value;

use crate::account_service::AuthorizationContext;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use crate::providers::qoder::credential::QoderCredential;
use crate::providers::qoder::http::QoderApi;
use crate::providers::qoder::region;

const ACTION_CLAIM_BENEFIT: &str = "CLAIM_BENEFIT";
const STATUS_CLAIMABLE: &str = "CLAIMABLE";
const STATUS_CLAIMED: &str = "CLAIMED";
const STATUS_BLOCKED: &str = "BLOCKED";
const SAME_PERSON: &str = "SAME_PERSON_ALREADY_CLAIMED";
/// Required for the campaign family; without it the upstream degrades silently.
const CAMPAIGN_CLIENT_TYPE: &str = "10";

pub struct QoderCheckinExecutor {
    api: QoderApi,
    claim_enabled: bool,
    /// Native device identity from Qoder's own `runtime-info` helper, supplied
    /// by the operator. The Intl campaign family only lists the daily reward
    /// when the request carries it; a synthetic per-uid identity does not work
    /// (and hides the CN campaign too).
    machine: Option<(String, String)>,
}

impl QoderCheckinExecutor {
    pub fn new(region: Region, claim_enabled: bool, client: reqwest::Client) -> Self {
        Self {
            api: QoderApi::new(client, region),
            claim_enabled,
            machine: None,
        }
    }

    /// Operator-provided `machineToken` / `machineType` from the Qoder
    /// `runtime-info` helper. Never generated here.
    pub fn with_machine_identity(
        mut self,
        token: impl Into<String>,
        machine_type: impl Into<String>,
    ) -> Self {
        let token = token.into().trim().to_string();
        let machine_type = machine_type.into().trim().to_string();
        if !token.is_empty() && !machine_type.is_empty() {
            self.machine = Some((token, machine_type));
        }
        self
    }

    fn campaign_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![("cosy-clienttype", CAMPAIGN_CLIENT_TYPE.to_string())];
        if let Some((token, machine_type)) = &self.machine {
            headers.push(("cosy-machinetoken", token.clone()));
            headers.push(("cosy-machinetype", machine_type.clone()));
        }
        headers
    }

    /// Tests only.
    pub fn with_base(
        region: Region,
        claim_enabled: bool,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self {
            api: QoderApi::with_base(client, region, base),
            claim_enabled,
            machine: None,
        }
    }

    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        self.api.set_identity(identity);
        self
    }

    pub fn claim_enabled(&self) -> bool {
        self.claim_enabled
    }

    fn credential(authorization: &AuthorizationContext) -> Result<QoderCredential, GatewayError> {
        let credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("Qoder account has no credential"))?;
        QoderCredential::from_credential(credential, authorization.region)
    }

    async fn campaigns(&self, credential: &QoderCredential) -> Result<Value, GatewayError> {
        let extra = self.campaign_headers();
        let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match self
            .api
            .get_with_headers(region::CAMPAIGNS_PATH, credential, &extra)
            .await
        {
            Ok(payload) => Ok(payload),
            Err(error) if is_unavailable(&error) => Err(GatewayError::unsupported(
                "Qoder campaign API is not available in this region",
            )),
            Err(error) => Err(error),
        }
    }

    async fn claim(
        &self,
        credential: &QoderCredential,
        campaign_id: &str,
    ) -> Result<Value, GatewayError> {
        // Validates the id before it is interpolated into the path.
        region::realm(credential.realm).campaign_claim_url(campaign_id)?;
        let path = region::CAMPAIGN_CLAIM_PATH.replace("{campaign_id}", campaign_id);
        let extra = self.campaign_headers();
        let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match self
            .api
            .post_with_headers(
                &path,
                &Value::Object(Default::default()),
                credential,
                &extra,
            )
            .await
        {
            Ok(payload) => Ok(payload),
            Err(error) if is_unavailable(&error) => Err(GatewayError::unsupported(
                "Qoder campaign claim API is not available in this region",
            )),
            Err(error) => Err(error),
        }
    }
}

fn is_unavailable(error: &GatewayError) -> bool {
    matches!(error.upstream_status, Some(404 | 405 | 410))
}

/// Campañas that can be claimed today, in upstream order.
fn claimable_campaigns(payload: &Value) -> Vec<(&str, &Value)> {
    items(payload)
        .filter(|item| text(item, "actionType") == ACTION_CLAIM_BENEFIT)
        .filter(|item| text(item, "claimStatus") == STATUS_CLAIMABLE)
        .filter_map(|item| Some((text(item, "campaignId"), item)).filter(|(id, _)| !id.is_empty()))
        .collect()
}

fn has_campaigns(payload: &Value) -> bool {
    items(payload).any(|_| true)
}

/// Whether the list carries a daily check-in entry at all (claimable, claimed
/// or otherwise). A targeted account always has one; an account the campaign
/// is not delivered to keeps only the global `VIEW_DETAILS` promo.
fn has_daily_campaign(payload: &Value) -> bool {
    items(payload).any(|item| text(item, "actionType") == ACTION_CLAIM_BENEFIT)
}

/// The upstream sets this to `true` when the account is targeted by any
/// campaign. When it is `false` the empty list is genuine; when it is `true` an
/// empty list means the response was degraded, not that the account is done.
fn show_campaign(payload: &Value) -> bool {
    payload
        .get("showCampaign")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn already_claimed(payload: &Value) -> bool {
    items(payload)
        .filter(|item| text(item, "actionType") == ACTION_CLAIM_BENEFIT)
        .any(|item| text(item, "claimStatus") == STATUS_CLAIMED)
}

fn items(payload: &Value) -> impl Iterator<Item = &Value> {
    payload
        .get("campaigns")
        .and_then(Value::as_array)
        .map(|list| list.iter())
        .into_iter()
        .flatten()
        .filter(|item| item.is_object())
}

fn text<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn claim_credit(payload: &Value) -> Option<f64> {
    let benefit = payload.get("benefit")?;
    if let Some(kind) = benefit.get("kind").and_then(Value::as_str) {
        if !kind.is_empty() && kind != "CREDITS" {
            return None;
        }
    }
    benefit.get("amount").and_then(Value::as_f64)
}

#[async_trait]
impl CheckinExecutor for QoderCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Qoder
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        let credential = Self::credential(authorization)?;
        let payload = self.campaigns(&credential).await?;
        if !has_campaigns(&payload) {
            if show_campaign(&payload) {
                // `showCampaign=true` with an empty list is the upstream's
                // silent degradation (for example a dropped `cosy-clienttype`),
                // not a settled "nothing to claim".
                return Ok(CheckinProbe::Unknown);
            }
            return Err(GatewayError::unsupported(
                "Qoder reports no daily check-in campaign for this account",
            ));
        }
        if !claimable_campaigns(&payload).is_empty() {
            return Ok(CheckinProbe::Unknown);
        }
        if already_claimed(&payload) {
            return Ok(CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn));
        }
        if !has_daily_campaign(&payload) {
            // The account is served campaigns but not the daily check-in.
            return Ok(CheckinProbe::confirmed(CheckinStatus::Unsupported));
        }
        Ok(CheckinProbe::Unknown)
    }

    async fn checkin(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinOutcome, GatewayError> {
        let credential = Self::credential(authorization)?;
        let payload = self.campaigns(&credential).await?;
        if !has_campaigns(&payload) {
            if show_campaign(&payload) {
                return Ok(CheckinOutcome::Failed {
                    category: CheckinErrorCategory::Upstream,
                    summary: "Qoder listed no campaigns although the campaign page is enabled; \
                              the response was degraded upstream, retry later"
                        .into(),
                });
            }
            return Err(GatewayError::unsupported(
                "Qoder exposed no campaign page for this account (showCampaign is false)",
            ));
        }
        let claimable = claimable_campaigns(&payload);
        if claimable.is_empty() {
            if already_claimed(&payload) {
                return Ok(CheckinOutcome::AlreadyCheckedIn {
                    summary: "Qoder reports today as already checked in".into(),
                });
            }
            if !has_daily_campaign(&payload) {
                return Err(GatewayError::unsupported(
                    "Qoder did not target this account with a daily check-in campaign",
                ));
            }
            return Ok(CheckinOutcome::Unknown {
                summary: "Qoder lists a daily check-in campaign in an unrecognized state; \
                          retry later"
                    .into(),
            });
        }
        if !self.claim_enabled {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "Qoder reward claim is disabled; set the provider option \
                          `checkin.claim_enabled = true` to allow claims"
                    .into(),
            });
        }

        let (campaign_id, _) = claimable[0];
        let response = self.claim(&credential, campaign_id).await?;
        if response.get("replayed").and_then(Value::as_bool) == Some(true)
            || (text(&response, "status") == STATUS_BLOCKED
                && text(&response, "failureCode") == SAME_PERSON)
        {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "Qoder reports today's reward as already claimed".into(),
            });
        }
        if text(&response, "status") == STATUS_BLOCKED {
            let code = {
                let failure = text(&response, "failureCode");
                if failure.is_empty() {
                    "BLOCKED"
                } else {
                    failure
                }
            };
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: format!("Qoder claim was rejected ({code})"),
            });
        }
        Ok(CheckinOutcome::Succeeded {
            summary: match claim_credit(&response) {
                Some(credit) => format!("checked in; Qoder granted {credit} credits"),
                None => "checked in; Qoder did not report the reward amount".into(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;
    use crate::config::Region;
    use crate::error::ErrorKind;
    use serde_json::json;

    use crate::test_support::{spawn_server, Reply};

    fn context(region: Region) -> AuthorizationContext {
        AuthorizationContext {
            account_id: "acct-1".into(),
            provider: format!("qoder-{}", region.as_str()),
            kind: ProviderKind::Qoder,
            region,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(json!({
                "access_token": "dt-access",
                "uid": "uid-1",
            }))),
            credential_revision: Some(0),
        }
    }

    fn request() -> CheckinRequest {
        CheckinRequest {
            account_id: "acct-1".into(),
            kind: ProviderKind::Qoder,
            region: Region::Cn,
            business_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn campaigns() -> String {
        json!({
            "campaigns": [
                {"campaignId": "c-1", "actionType": "VIEW_DETAILS", "claimStatus": "CLAIMABLE"},
                {"campaignId": "c-2", "actionType": "CLAIM_BENEFIT", "claimStatus": "CLAIMABLE",
                 "benefit": {"kind": "CREDITS", "amount": 5}},
            ]
        })
        .to_string()
    }

    #[tokio::test]
    async fn claim_requires_the_explicit_option() {
        let server = spawn_server(|request| match request.path() {
            "/sash/api/v1/me/campaigns" => Reply::json(200, campaigns()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            false,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                ..
            }
        ));
        assert_eq!(server.request_count(), 1, "no claim must be sent");
    }

    #[tokio::test]
    async fn claim_sends_client_type_and_returns_credit() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("GET", "/sash/api/v1/me/campaigns") => Reply::json(200, campaigns()),
            ("POST", "/sash/api/v1/me/campaigns/c-2/claim") => Reply::json(
                200,
                json!({"status": "SUCCESS", "benefit": {"amount": 5}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(), &context(Region::Cn))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CheckinOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        let requests = server.requests();
        assert!(requests
            .iter()
            .all(|request| request.header("cosy-clienttype") == Some("10")));
        let claim = requests
            .iter()
            .find(|request| request.method == "POST")
            .expect("claim posted");
        assert_eq!(claim.header("authorization"), Some("Bearer dt-access"));
        assert_eq!(claim.body.trim(), "{}");
    }

    #[tokio::test]
    async fn machine_identity_is_sent_on_campaigns_and_claim() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("GET", "/sash/api/v1/me/campaigns") => Reply::json(200, campaigns()),
            ("POST", "/sash/api/v1/me/campaigns/c-2/claim") => Reply::json(
                200,
                json!({"status": "SUCCESS", "benefit": {"amount": 100}}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        )
        .with_machine_identity("native-token", "native-type");
        let outcome = executor
            .checkin(&request(), &context(Region::Intl))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CheckinOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        let requests = server.requests();
        assert!(!requests.is_empty());
        for request in requests {
            assert_eq!(request.header("cosy-clienttype"), Some("10"));
            assert_eq!(request.header("cosy-machinetoken"), Some("native-token"));
            assert_eq!(request.header("cosy-machinetype"), Some("native-type"));
        }
    }

    #[tokio::test]
    async fn without_machine_identity_only_the_client_type_is_sent() {
        let server = spawn_server(|request| match request.path() {
            "/sash/api/v1/me/campaigns" => Reply::json(200, campaigns()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            false,
            reqwest::Client::new(),
            &server.base_url,
        );
        let _ = executor.checkin(&request(), &context(Region::Intl)).await;
        let requests = server.requests();
        assert!(!requests.is_empty());
        for request in requests {
            assert_eq!(request.header("cosy-clienttype"), Some("10"));
            assert_eq!(request.header("cosy-machinetoken"), None);
            assert_eq!(request.header("cosy-machinetype"), None);
        }
    }

    #[tokio::test]
    async fn same_person_and_replay_are_already_checked_in() {
        for body in [
            json!({"status": "BLOCKED", "failureCode": "SAME_PERSON_ALREADY_CLAIMED"}),
            json!({"replayed": true}),
        ] {
            let body = body.to_string();
            let reply = body.clone();
            let server =
                spawn_server(
                    move |request| match (request.method.as_str(), request.path()) {
                        ("GET", "/sash/api/v1/me/campaigns") => Reply::json(200, campaigns()),
                        ("POST", "/sash/api/v1/me/campaigns/c-2/claim") => {
                            Reply::json(200, reply.clone())
                        }
                        _ => Reply::json(404, "{}"),
                    },
                )
                .await;
            let executor = QoderCheckinExecutor::with_base(
                Region::Cn,
                true,
                reqwest::Client::new(),
                &server.base_url,
            );
            let outcome = executor
                .checkin(&request(), &context(Region::Cn))
                .await
                .unwrap();
            assert!(
                matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }),
                "{body} -> {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn other_blocked_codes_are_failures() {
        let server = spawn_server(|request| match (request.method.as_str(), request.path()) {
            ("GET", "/sash/api/v1/me/campaigns") => Reply::json(200, campaigns()),
            ("POST", "/sash/api/v1/me/campaigns/c-2/claim") => Reply::json(
                200,
                json!({"status": "BLOCKED", "failureCode": "BUDGET_EXHAUSTED"}).to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn display_only_campaign_is_not_claimed() {
        let server = spawn_server(|request| match request.path() {
            "/sash/api/v1/me/campaigns" => Reply::json(
                200,
                json!({"campaigns": [
                    {"campaignId": "c-1", "actionType": "VIEW_DETAILS", "claimStatus": "CLAIMABLE"}
                ]})
                .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let error = executor
            .checkin(&request(), &context(Region::Cn))
            .await
            .expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(
            error.message.contains("did not target"),
            "{}",
            error.message
        );
        assert_eq!(server.request_count(), 1);
    }

    #[tokio::test]
    async fn an_account_without_the_daily_campaign_is_settled_unsupported() {
        // Shape of `Untitledtest1.trace` (Intl, 2026-11-01): the account gets
        // only the global subscription promo, yet the growth page is on.
        let server = spawn_server(|_| {
            Reply::json(
                200,
                json!({"uid": "01a18cd3-c3da-726e-8ce2-7766be8e3dba",
                       "showCampaign": true, "claimable": false,
                       "campaignUrl": "https://openapi.qoder.sh/growth-page/activity-iframe",
                       "campaigns": [{
                           "campaignId": "01a159af-8a04-76b8-b0f4-59d8e2b60583",
                           "campaignKey": "act-20261020-944",
                           "actionType": "VIEW_DETAILS", "claimStatus": "CLAIMED"}]})
                .to_string(),
            )
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let error = executor
            .checkin(&request(), &context(Region::Intl))
            .await
            .expect_err("not targeted");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(
            error.message.contains("did not target"),
            "{}",
            error.message
        );

        let probe = executor
            .check_status(&request(), &context(Region::Intl))
            .await
            .expect("probe");
        assert_eq!(probe, CheckinProbe::confirmed(CheckinStatus::Unsupported));
        assert!(server
            .requests()
            .iter()
            .all(|request| request.method == "GET"));
    }

    #[tokio::test]
    async fn a_daily_campaign_in_an_unknown_state_is_not_claimed() {
        let server = spawn_server(|request| match request.path() {
            "/sash/api/v1/me/campaigns" => Reply::json(
                200,
                json!({"showCampaign": true, "campaigns": [
                    {"campaignId": "c-2", "actionType": "CLAIM_BENEFIT", "claimStatus": "BLOCKED"}
                ]})
                .to_string(),
            ),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(), &context(Region::Cn))
            .await
            .unwrap();
        assert!(
            matches!(outcome, CheckinOutcome::Unknown { .. }),
            "{outcome:?}"
        );
        assert_eq!(server.request_count(), 1, "no claim on an unknown state");
    }

    #[tokio::test]
    async fn an_empty_list_with_the_campaign_page_enabled_is_a_retryable_failure() {
        let server = spawn_server(|_| {
            Reply::json(
                200,
                json!({"uid": "u", "showCampaign": true, "claimable": true,
                       "campaignUrl": "https://openapi.qoder.sh/growth-page/activity-iframe",
                       "campaigns": []})
                .to_string(),
            )
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(), &context(Region::Intl))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                ..
            }
        ));
        assert_eq!(server.request_count(), 1, "no claim on a degraded list");

        let probe = executor
            .check_status(&request(), &context(Region::Intl))
            .await
            .expect("probe");
        assert_eq!(probe, CheckinProbe::Unknown);
    }

    #[tokio::test]
    async fn an_empty_list_without_targeting_is_unsupported() {
        let server = spawn_server(|_| {
            Reply::json(
                200,
                json!({"uid": "u", "showCampaign": false, "claimable": false,
                       "campaignUrl": "", "campaigns": []})
                .to_string(),
            )
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let error = executor
            .checkin(&request(), &context(Region::Intl))
            .await
            .expect_err("unsupported");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("showCampaign"), "{}", error.message);
    }

    #[tokio::test]
    async fn missing_campaign_endpoint_is_unsupported() {
        let server = spawn_server(|_| Reply::json(404, "{}")).await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Intl,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let error = executor
            .checkin(&request(), &context(Region::Intl))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        let probe = executor
            .check_status(&request(), &context(Region::Intl))
            .await
            .unwrap_err();
        assert_eq!(probe.kind, ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn claimed_campaign_probes_as_already_checked_in() {
        let server = spawn_server(|_| {
            Reply::json(
                200,
                json!({"campaigns": [
                    {"campaignId": "c-2", "actionType": "CLAIM_BENEFIT", "claimStatus": "CLAIMED"}
                ]})
                .to_string(),
            )
        })
        .await;
        let executor = QoderCheckinExecutor::with_base(
            Region::Cn,
            true,
            reqwest::Client::new(),
            &server.base_url,
        );
        let probe = executor
            .check_status(&request(), &context(Region::Cn))
            .await
            .unwrap();
        assert_eq!(
            probe,
            CheckinProbe::confirmed(CheckinStatus::AlreadyCheckedIn)
        );
    }
}
