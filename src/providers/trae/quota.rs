//! TRAE quota parsing.
//!
//! Evidence: `docs/TRAE_MIGRATION.md` §10. Two references disagree on the
//! endpoint, so the primary (Coding2API) path is implemented:
//!
//! ```text
//! POST {pay_base}/trae/api/v2/pay/ide_user_ent_usage   body {}
//! user_entitlement_pack_list[]                          (CN credits)
//!   entitlement_base_info.quota.credits_limit
//!   usage.credits_amount
//!   entitlement_base_info.end_time / expire_time
//! ```
//!
//! dsh additionally reads `usage_summary.total_amount` / `consumed_amount`
//! from `web_user_ent_usage`; those fields are honoured when present, but the
//! endpoint stays Coding2API's. The International region has no credit packs —
//! its `ide_user_pay_status` answer only names a plan, so its snapshot carries
//! the plan in `raw` and leaves the numbers `null` (unknown, never 0).
//!
//! Units are TRAE credits; no token conversion is invented. A package keeps its
//! own expiry, so the aggregate's period is "per_package" rather than a single
//! cycle.

use chrono::Utc;
use serde_json::{json, Value};

use crate::config::Region;
use crate::error::GatewayError;
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

use super::error::is_business_failure;

pub fn usage_request_body() -> Value {
    // Coding2API sends an empty object to `ide_user_ent_usage`.
    json!({})
}

pub fn pay_status_request_body() -> Value {
    json!({})
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraePack {
    pub name: String,
    pub total: f64,
    pub used: f64,
    pub remaining: f64,
    pub expires_at: Option<i64>,
}

pub fn parse_usage(
    region: Region,
    payload: &Value,
    now: i64,
) -> Result<QuotaSnapshot, GatewayError> {
    if is_business_failure(&payload.to_string()) {
        return Err(GatewayError::upstream_server(
            "TRAE quota returned a business error",
        ));
    }
    let packs = parse_packs(payload);
    let summary = usage_summary(payload);

    let (total, remaining) = match summary {
        Some((total, consumed)) => {
            let remaining = (total - consumed).max(0.0);
            let mut snapshot = known_snapshot(total, remaining);
            snapshot.raw = Some(raw_usage(region, now, &packs, Some((total, consumed))));
            return Ok(snapshot);
        }
        None if !packs.is_empty() => {
            let total: f64 = packs.iter().map(|pack| pack.total).sum();
            let remaining: f64 = packs.iter().map(|pack| pack.remaining).sum();
            (total, remaining.min(total))
        }
        None => {
            let mut snapshot = QuotaSnapshot::unknown("", "TRAE returned no active credit package");
            snapshot.unit = QuotaUnit::Credits;
            snapshot.raw = Some(raw_usage(region, now, &[], None));
            return Ok(snapshot);
        }
    };

    let mut snapshot = known_snapshot(total, remaining.clamp(0.0, total.max(0.0)));
    snapshot.raw = Some(raw_usage(region, now, &packs, None));
    Ok(snapshot)
}

/// International pay status names the subscription, not a credit balance. The
/// plan is kept in `raw`; every numeric field stays `null`.
pub fn parse_pay_status(payload: &Value) -> QuotaSnapshot {
    let plan = payload
        .get("user_pay_identity_str")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("user_pay_identity")
                .and_then(Value::as_i64)
                .map(|tier| match tier {
                    0 => "Free".to_string(),
                    tier if tier > 0 => format!("Plan {tier}"),
                    _ => String::new(),
                })
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_default();
    let in_trial = payload
        .get("trial_status")
        .and_then(|trial| trial.get("is_in_trial"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut snapshot = QuotaSnapshot::unknown("", "");
    snapshot.unit = QuotaUnit::Unknown;
    snapshot.status = QuotaStatus::Unknown;
    snapshot.error = None;
    snapshot.raw = Some(json!({
        "source": "trae",
        "realm": Region::Intl.as_str(),
        "period": "subscription",
        "plan": plan,
        "in_trial": in_trial,
        "probed_at": Utc::now().timestamp(),
    }));
    snapshot
}

fn known_snapshot(total: f64, remaining: f64) -> QuotaSnapshot {
    let mut snapshot = QuotaSnapshot::unknown("", "");
    snapshot.unit = QuotaUnit::Credits;
    snapshot.status = QuotaStatus::Known;
    snapshot.total = Some(total);
    snapshot.remaining = Some(remaining);
    snapshot.error = None;
    snapshot
}

fn raw_usage(region: Region, now: i64, packs: &[TraePack], summary: Option<(f64, f64)>) -> Value {
    let packages: Vec<Value> = packs
        .iter()
        .map(|pack| {
            json!({
                "name": pack.name,
                "total": pack.total,
                "used": pack.used,
                "remaining": pack.remaining,
                "expires_at": pack.expires_at,
            })
        })
        .collect();
    // Only unexpired packs with a balance are spendable, so only those belong
    // in the expiry ladder.
    let mut ladder: Vec<(i64, f64)> = packs
        .iter()
        .filter(|pack| pack.remaining > 0.0)
        .filter_map(|pack| pack.expires_at.map(|end| (end, pack.remaining)))
        .filter(|(end, _)| *end > now)
        .collect();
    ladder.sort_by_key(|(end, _)| *end);
    json!({
        "source": "trae",
        "realm": region.as_str(),
        "period": "per_package",
        "probed_at": now,
        "unit": "credits",
        "packages": packages,
        "expiry_ladder": ladder
            .into_iter()
            .map(|(end, remaining)| json!({"expires_at": end, "remaining": remaining}))
            .collect::<Vec<_>>(),
        "usage_summary": summary.map(|(total, consumed)| json!({
            "total_amount": total,
            "consumed_amount": consumed,
        })),
    })
}

fn parse_packs(payload: &Value) -> Vec<TraePack> {
    let Some(list) = payload
        .get("user_entitlement_pack_list")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut packs = Vec::new();
    for entry in list {
        let Some(pack) = entry.as_object() else {
            continue;
        };
        if pack.get("is_hide").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let base = pack.get("entitlement_base_info").and_then(Value::as_object);
        let total = base
            .and_then(|base| base.get("quota"))
            .and_then(Value::as_object)
            .and_then(|quota| number(quota.get("credits_limit")))
            .filter(|total| *total > 0.0);
        let Some(total) = total else {
            continue;
        };
        let used = pack
            .get("usage")
            .and_then(Value::as_object)
            .and_then(|usage| number(usage.get("credits_amount")))
            .unwrap_or(0.0)
            .max(0.0);
        let remaining = (total - used).max(0.0);
        let expires_at = base
            .and_then(|base| base.get("end_time"))
            .and_then(epoch_of)
            .or_else(|| pack.get("expire_time").and_then(epoch_of));
        packs.push(TraePack {
            name: pack_name(pack, base),
            total,
            used,
            remaining,
            expires_at,
        });
    }
    packs
}

/// `usage_summary` is not part of the Coding2API endpoint; when a build returns
/// it, its totals are the upstream's own answer and take precedence.
fn usage_summary(payload: &Value) -> Option<(f64, f64)> {
    let summary = payload.get("usage_summary")?.as_object()?;
    let total = number(summary.get("total_amount"))?;
    let consumed = number(summary.get("consumed_amount"))?;
    Some((total, consumed))
}

fn pack_name(
    pack: &serde_json::Map<String, Value>,
    base: Option<&serde_json::Map<String, Value>>,
) -> String {
    if let Some(group) = pack.get("group_name").and_then(Value::as_str) {
        let group = group.trim();
        if !group.is_empty() {
            return group.to_string();
        }
    }
    if let Some(name) = base
        .and_then(|base| base.get("product_extra"))
        .and_then(Value::as_object)
        .and_then(|extra| extra.get("package_extra"))
        .and_then(Value::as_object)
        .and_then(|extra| extra.get("package_name"))
        .and_then(Value::as_str)
    {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    pack.get("display_desc")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("credits")
        .to_string()
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

fn epoch_of(value: &Value) -> Option<i64> {
    let raw = match value {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    if raw <= 0 {
        return None;
    }
    Some(if raw > 100_000_000_000 {
        raw / 1000
    } else {
        raw
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(packs: Value, summary: Option<Value>) -> Value {
        let mut body = json!({"user_entitlement_pack_list": packs});
        if let Some(summary) = summary {
            body["usage_summary"] = summary;
        }
        body
    }

    #[test]
    fn packs_sum_into_remaining_and_keep_expiry() {
        let body = payload(
            json!([
                {
                    "group_name": "每月登录积分",
                    "entitlement_base_info": {
                        "quota": {"credits_limit": 100},
                        "end_time": 2_000_000_000_i64
                    },
                    "usage": {"credits_amount": 40}
                },
                {
                    "display_desc": "福利积分",
                    "entitlement_base_info": {
                        "quota": {"credits_limit": "50"},
                        "end_time": 1_900_000_000_i64
                    },
                    "usage": {"credits_amount": "10"}
                }
            ]),
            None,
        );
        let snapshot = parse_usage(Region::Cn, &body, 1_000).unwrap();
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.total, Some(150.0));
        assert_eq!(snapshot.remaining, Some(100.0));
        let raw = snapshot.raw.unwrap();
        assert_eq!(raw["period"], "per_package");
        assert_eq!(raw["packages"].as_array().unwrap().len(), 2);
        assert_eq!(raw["expiry_ladder"].as_array().unwrap().len(), 2);
        assert_eq!(raw["packages"][0]["name"], "每月登录积分");
    }

    #[test]
    fn a_summary_wins_over_the_pack_sum() {
        let body = payload(
            json!([{
                "entitlement_base_info": {"quota": {"credits_limit": 100}},
                "usage": {"credits_amount": 40}
            }]),
            Some(json!({"total_amount": 200, "consumed_amount": 150})),
        );
        let snapshot = parse_usage(Region::Cn, &body, 1_000).unwrap();
        assert_eq!(snapshot.total, Some(200.0));
        assert_eq!(snapshot.remaining, Some(50.0));
        let raw = snapshot.raw.unwrap();
        assert_eq!(raw["usage_summary"]["total_amount"], 200.0);
    }

    #[test]
    fn hidden_and_empty_packs_are_ignored() {
        let body = payload(
            json!([
                {"is_hide": true, "entitlement_base_info": {"quota": {"credits_limit": 999}}},
                {"entitlement_base_info": {"quota": {"credits_limit": 0}}},
                {"entitlement_base_info": {"quota": {"credits_limit": "bad"}}}
            ]),
            None,
        );
        let snapshot = parse_usage(Region::Cn, &body, 1_000).unwrap();
        assert_eq!(snapshot.status, QuotaStatus::Unknown);
        assert_eq!(snapshot.total, None);
        assert_eq!(snapshot.remaining, None);
    }

    #[test]
    fn an_exhausted_pack_is_known_zero_not_unknown() {
        let body = payload(
            json!([{
                "entitlement_base_info": {"quota": {"credits_limit": 10}},
                "usage": {"credits_amount": 10}
            }]),
            None,
        );
        let snapshot = parse_usage(Region::Cn, &body, 1_000).unwrap();
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.remaining, Some(0.0));
    }

    #[test]
    fn a_business_error_is_not_an_empty_balance() {
        let error = parse_usage(Region::Cn, &json!({"code": 14018}), 1_000).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }

    #[test]
    fn intl_pay_status_keeps_the_plan_without_inventing_numbers() {
        let snapshot = parse_pay_status(&json!({
            "user_pay_identity_str": "Pro",
            "trial_status": {"is_in_trial": true}
        }));
        assert_eq!(snapshot.status, QuotaStatus::Unknown);
        assert_eq!(snapshot.unit, QuotaUnit::Unknown);
        assert!(snapshot.total.is_none() && snapshot.remaining.is_none());
        let raw = snapshot.raw.unwrap();
        assert_eq!(raw["plan"], "Pro");
        assert_eq!(raw["in_trial"], true);
        assert_eq!(raw["period"], "subscription");
    }

    #[test]
    fn a_numeric_tier_is_labelled() {
        let snapshot = parse_pay_status(&json!({"user_pay_identity": 2}));
        assert_eq!(snapshot.raw.unwrap()["plan"], "Plan 2");
    }
}
