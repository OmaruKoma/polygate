//! TRAE daily check-in executor (CN only).
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §11 and the sources it cites:
//! Coding2API `src/provider/trae/client.py:461-476` (status/claim) and
//! `events.py:43-44` (paths); dsh `trae-upstream.ts:696-720` (`x-device-id`
//! must be the persisted 16-digit device number; a GUID answers 9074) and
//! `host.ts:5916` (one check-in per device per day).
//!
//! ```text
//! POST {pay_base}/trae/api/v2/ug/checkin_credits/status  body {}
//! POST {pay_base}/trae/api/v2/ug/checkin_credits/claim   body {}
//! ```
//!
//! Compliance boundary: the reference generates a fresh numeric device id for
//! each claim. This executor **never generates one**. It requires the operator
//! to configure a concrete device number (`checkin.device_id`); without it the
//! check-in is reported as a configuration failure instead of fabricating a
//! device identity.
//!
//! Claim confirmation follows the reference discipline: `code: 0` alone is not
//! success (an already-claimed account answers 0 idempotently). The executor
//! re-reads the status and requires `checked_in` plus a credit increase. Claim
//! codes change account state, so claims only run when `checkin.claim_enabled`
//! is set.

use async_trait::async_trait;
use serde_json::Value;

use crate::account_service::AuthorizationContext;
use crate::checkin::{
    CheckinErrorCategory, CheckinExecutor, CheckinOutcome, CheckinProbe, CheckinRequest,
    CheckinStatus,
};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use crate::providers::trae::credential::TraeCredential;
use crate::providers::trae::error::classify_http_status;
use crate::providers::trae::http::TraeHttp;
use crate::providers::trae::region;

const CODE_DEVICE_ALREADY_CLAIMED: i64 = 9095;
const CODE_TOO_MANY_PARTICIPANTS: i64 = 9074;
const DEVICE_DIGITS: usize = 16;

pub struct TraeCheckinExecutor {
    http: TraeHttp,
    region: Region,
    claim_enabled: bool,
    device_id: Option<String>,
}

impl TraeCheckinExecutor {
    pub fn new(
        region: Region,
        claim_enabled: bool,
        device_id: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            http: TraeHttp::new(client, region),
            region,
            claim_enabled,
            device_id: valid_device_id(device_id.as_deref()),
        }
    }

    /// Tests only.
    pub fn with_base(
        region: Region,
        claim_enabled: bool,
        device_id: Option<String>,
        client: reqwest::Client,
        base: impl Into<String>,
    ) -> Self {
        Self {
            http: TraeHttp::with_base(client, region, base),
            region,
            claim_enabled,
            device_id: valid_device_id(device_id.as_deref()),
        }
    }

    pub fn with_identity(mut self, identity: crate::providers::profile::Identity) -> Self {
        self.http.set_identity(identity);
        self
    }

    pub fn claim_enabled(&self) -> bool {
        self.claim_enabled
    }

    pub fn has_device_id(&self) -> bool {
        self.device_id.is_some()
    }

    fn credential(
        &self,
        authorization: &AuthorizationContext,
    ) -> Result<TraeCredential, GatewayError> {
        if self.region != authorization.region {
            return Err(GatewayError::unsupported(
                "TRAE check-in is not available for this account's region",
            ));
        }
        let credential = authorization
            .credential
            .as_ref()
            .ok_or_else(|| GatewayError::auth_invalid("TRAE account has no credential"))?;
        TraeCredential::from_credential(credential, authorization.region)
    }

    fn usable(&self) -> Result<&str, GatewayError> {
        self.device_id.as_deref().ok_or_else(|| {
            GatewayError::bad_request(
                "TRAE check-in requires the provider option `checkin.device_id` \
                 (a 16-digit device number); PolyGate does not generate one",
            )
        })
    }

    fn empty() -> Value {
        Value::Object(Default::default())
    }

    async fn status(&self, credential: &TraeCredential) -> Result<StatusView, GatewayError> {
        let payload = self
            .http
            .post_pay_json(
                region::CHECKIN_STATUS_PATH,
                &Self::empty(),
                credential,
                self.device_id.as_deref(),
            )
            .await?;
        parse_status(&payload)
    }

    async fn claim(&self, credential: &TraeCredential) -> Result<(u16, Value), GatewayError> {
        let (status, body) = self
            .http
            .post_pay_raw(
                region::CHECKIN_CLAIM_PATH,
                &Self::empty(),
                credential,
                self.device_id.as_deref(),
            )
            .await?;
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            GatewayError::upstream_server("TRAE check-in returned unparsable JSON")
                .with_upstream_status(status)
        })?;
        Ok((status, payload))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct StatusView {
    enable: bool,
    checked_in: bool,
    did_checked_in: bool,
    credits: Option<f64>,
}

fn parse_status(payload: &Value) -> Result<StatusView, GatewayError> {
    if let Some(code) = business_code(payload) {
        if code != 0 {
            return Err(GatewayError::upstream_server(format!(
                "TRAE check-in status returned code {code}"
            )));
        }
    }
    let record = payload
        .as_object()
        .ok_or_else(|| GatewayError::upstream_server("TRAE check-in status is not an object"))?;
    Ok(StatusView {
        enable: enabled(record.get("enable")),
        checked_in: flag(record.get("checked_in")),
        did_checked_in: flag(record.get("did_checked_in")),
        credits: number(record.get("credits")),
    })
}

/// Only an explicit off disables check-in: the upstream omits the field for
/// accounts that have it on.
fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_i64() != Some(0),
        Some(Value::String(text)) => {
            let text = text.trim().to_ascii_lowercase();
            text != "false" && text != "0"
        }
        None => true,
        _ => true,
    }
}

fn flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_i64().is_some_and(|value| value != 0),
        Some(Value::String(text)) => text.trim().eq_ignore_ascii_case("true") || text.trim() == "1",
        _ => false,
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    if let Some(number) = value.as_f64() {
        return number.is_finite().then_some(number);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|number| number.is_finite())
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
        .get("message")
        .or_else(|| payload.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn valid_device_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.len() == DEVICE_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.to_string())
}

#[async_trait]
impl CheckinExecutor for TraeCheckinExecutor {
    fn supports(&self, kind: ProviderKind) -> bool {
        kind == ProviderKind::Trae
    }

    async fn check_status(
        &self,
        _request: &CheckinRequest,
        authorization: &AuthorizationContext,
    ) -> Result<CheckinProbe, GatewayError> {
        if self.region == Region::Intl {
            return Err(GatewayError::unsupported(
                "TRAE check-in is not supported on the international realm",
            ));
        }
        let credential = self.credential(authorization)?;
        self.usable()?;
        let status = self.status(&credential).await?;
        if !status.enable {
            return Err(GatewayError::unsupported(
                "TRAE reports no active check-in programme for this account",
            ));
        }
        if status.checked_in {
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
        if self.device_id.is_none() {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "TRAE check-in is disabled until the provider option \
                          `checkin.device_id` is set to the 16-digit device number; \
                          PolyGate does not generate device identities"
                    .into(),
            });
        }
        let status = self.status(&credential).await?;
        if !status.enable {
            return Ok(CheckinOutcome::Unsupported);
        }
        if status.checked_in {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "TRAE reports today as already checked in".into(),
            });
        }
        if status.did_checked_in {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "this TRAE device already claimed today's check-in".into(),
            });
        }
        if !self.claim_enabled {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Other,
                summary: "TRAE reward claim is disabled; set the provider option \
                          `checkin.claim_enabled = true` to allow claims"
                    .into(),
            });
        }
        let before = status;
        let (http_status, payload) = self.claim(&credential).await?;
        if http_status == 401 || http_status == 403 {
            return Err(classify_http_status(http_status, &payload.to_string()));
        }
        let code = business_code(&payload);
        if code == Some(CODE_DEVICE_ALREADY_CLAIMED) {
            return Ok(CheckinOutcome::AlreadyCheckedIn {
                summary: "this TRAE device already claimed today's check-in".into(),
            });
        }
        if code == Some(CODE_TOO_MANY_PARTICIPANTS) {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::RateLimit,
                summary: "TRAE refused the claim with 9074 (device/participant limit); \
                          retry later with the same device id"
                    .into(),
            });
        }
        if code != Some(0) {
            if http_status >= 400 {
                return Err(classify_http_status(http_status, &payload.to_string()));
            }
            let label = code.map_or_else(
                || "without a business code".to_string(),
                |code| format!("with code {code}"),
            );
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: format!("TRAE declined the claim {label} {}", message_of(&payload))
                    .trim()
                    .to_string(),
            });
        }

        // `code: 0` is not proof: re-read the status and require both flags and
        // a credit increase. A re-read failure stays an error (Unknown), since
        // the claim may already have been applied.
        let after = self.status(&credential).await?;
        if !after.checked_in {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: "TRAE accepted the claim but status still reports today as \
                          not checked in"
                    .into(),
            });
        }
        let gained = match (before.credits, after.credits) {
            (Some(before), Some(after)) => after - before,
            (None, Some(after)) => after,
            _ => 0.0,
        };
        if gained <= 0.0 {
            return Ok(CheckinOutcome::Failed {
                category: CheckinErrorCategory::Upstream,
                summary: "TRAE accepted the claim but no credit increase was observed".into(),
            });
        }
        Ok(CheckinOutcome::Succeeded {
            summary: format!("checked in; TRAE granted {gained} credits"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Credential;
    use crate::test_support::{spawn_server, Reply};
    use serde_json::json;

    const DEVICE: &str = "0123456789012345";

    fn context(region: Region) -> AuthorizationContext {
        AuthorizationContext {
            account_id: "acct-1".into(),
            provider: format!("trae-{}", region.as_str()),
            kind: ProviderKind::Trae,
            region,
            enabled: true,
            auth_status: crate::accounts::AuthStatus::Valid,
            credential: Some(Credential::from_value(json!({
                "access_token": "jwt-access",
                "uid": "uid-1",
                "domain": "trae.cn",
                "machineId": "machine-1",
            }))),
            credential_revision: Some(0),
        }
    }

    fn request(region: Region) -> CheckinRequest {
        CheckinRequest {
            account_id: "acct-1".into(),
            kind: ProviderKind::Trae,
            region,
            business_date: chrono::NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            timezone: "Asia/Shanghai".into(),
            attempt: 1,
        }
    }

    fn status_body(enable: bool, checked: bool, did: bool, credits: f64) -> String {
        json!({
            "code": 0,
            "enable": enable,
            "checked_in": checked,
            "did_checked_in": did,
            "credits": credits
        })
        .to_string()
    }

    #[tokio::test]
    async fn claim_succeeds_only_after_the_credit_increase_is_confirmed() {
        // The second status read must show the new credit.
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter2 = counter.clone();
        let server = spawn_server(move |request| match request.path() {
            region::CHECKIN_STATUS_PATH => {
                let first = counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                Reply::json(
                    200,
                    status_body(true, !first, false, if first { 100.0 } else { 110.0 }),
                )
            }
            region::CHECKIN_CLAIM_PATH => Reply::json(200, json!({"code": 0}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
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
        assert!(summary.contains("10"), "{summary}");
        let claim = server
            .requests()
            .into_iter()
            .find(|request| request.path() == region::CHECKIN_CLAIM_PATH)
            .expect("claim posted");
        assert_eq!(
            claim.header("authorization"),
            Some("Cloud-IDE-JWT jwt-access")
        );
        assert_eq!(claim.header("x-device-id"), Some(DEVICE));
        assert_eq!(claim.header("x-user-region"), Some("CN"));
    }

    #[tokio::test]
    async fn a_missing_device_id_never_sends_a_request() {
        let server = spawn_server(|_| Reply::json(200, status_body(true, false, false, 0.0))).await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            None,
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
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn a_non_numeric_device_id_is_refused() {
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some("not-a-device".into()),
            reqwest::Client::new(),
            "http://127.0.0.1:9",
        );
        assert!(!executor.has_device_id());
    }

    #[tokio::test]
    async fn a_disabled_claim_never_posts() {
        let server = spawn_server(|request| match request.path() {
            region::CHECKIN_STATUS_PATH => Reply::json(200, status_body(true, false, false, 100.0)),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            false,
            Some(DEVICE.into()),
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
            .all(|request| request.path() != region::CHECKIN_CLAIM_PATH));
    }

    #[tokio::test]
    async fn an_already_checked_in_account_skips_the_claim() {
        let server = spawn_server(|request| match request.path() {
            region::CHECKIN_STATUS_PATH => Reply::json(200, status_body(true, true, false, 100.0)),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
        assert_eq!(server.request_count(), 1);
    }

    #[tokio::test]
    async fn a_spent_device_skips_the_claim() {
        let server = spawn_server(|request| match request.path() {
            region::CHECKIN_STATUS_PATH => Reply::json(200, status_body(true, false, true, 100.0)),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. }));
        assert_eq!(server.request_count(), 1);
    }

    #[tokio::test]
    async fn a_claim_without_a_credit_increase_is_not_success() {
        let server = spawn_server(|request| match request.path() {
            region::CHECKIN_STATUS_PATH => Reply::json(200, status_body(true, false, false, 100.0)),
            region::CHECKIN_CLAIM_PATH => Reply::json(200, json!({"code": 0}).to_string()),
            _ => Reply::json(404, "{}"),
        })
        .await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
            reqwest::Client::new(),
            &server.base_url,
        );
        let outcome = executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .unwrap();
        assert!(matches!(outcome, CheckinOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn claim_device_codes_are_mapped() {
        for (code, expected) in [(9095, "already"), (9074, "limit")] {
            let body = json!({"code": code, "message": "no"}).to_string();
            let reply = body.clone();
            let server = spawn_server(move |request| match request.path() {
                region::CHECKIN_STATUS_PATH => {
                    Reply::json(200, status_body(true, false, false, 100.0))
                }
                region::CHECKIN_CLAIM_PATH => Reply::json(200, reply.clone()),
                _ => Reply::json(404, "{}"),
            })
            .await;
            let executor = TraeCheckinExecutor::with_base(
                Region::Cn,
                true,
                Some(DEVICE.into()),
                reqwest::Client::new(),
                &server.base_url,
            );
            let outcome = executor
                .checkin(&request(Region::Cn), &context(Region::Cn))
                .await
                .unwrap();
            match expected {
                "already" => assert!(matches!(outcome, CheckinOutcome::AlreadyCheckedIn { .. })),
                _ => assert!(matches!(outcome, CheckinOutcome::Failed { .. })),
            }
        }
    }

    #[tokio::test]
    async fn an_inactive_programme_is_unsupported() {
        let server =
            spawn_server(|_| Reply::json(200, status_body(false, false, false, 0.0))).await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
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
        let server = spawn_server(|_| Reply::json(200, status_body(true, false, false, 0.0))).await;
        let executor = TraeCheckinExecutor::with_base(
            Region::Intl,
            true,
            Some(DEVICE.into()),
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
        let executor = TraeCheckinExecutor::with_base(
            Region::Cn,
            true,
            Some(DEVICE.into()),
            reqwest::Client::new(),
            "http://127.0.0.1:9",
        );
        assert!(executor
            .checkin(&request(Region::Cn), &context(Region::Cn))
            .await
            .is_err());
    }
}
