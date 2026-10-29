//! CodeArts credit balance parsing.
//!
//! Endpoint: `GET {snap}/snap-manager/v1/statistics/plugin` (the `snap-manager`
//! prefix is required; `/v1/ops/*` is the opposite — see
//! `docs/CHECKIN_RESEARCH.md`). Shape observed in Coding2API
//! `client.py::parse_credits` (2026-10-15):
//!
//! ```text
//! metrics[]: { name, package_credit_amount, package_credit_used,
//!              package_credit_remain, package_credit_expiring_amount }
//! ```
//!
//! `usageTotalPackageCredit` is the account total; the other entries are
//! sub-packages. A missing total is **unknown**, never zero.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

const TOTAL_METRIC: &str = "usageTotalPackageCredit";

pub fn parse_quota(account_id: &str, payload: &Value, now: DateTime<Utc>) -> QuotaSnapshot {
    let Some(metrics) = payload.get("metrics").and_then(Value::as_array) else {
        return QuotaSnapshot::unknown(
            account_id,
            "CodeArts statistics response has no metrics array",
        );
    };
    let Some(total) = metrics
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(TOTAL_METRIC))
    else {
        return QuotaSnapshot::unknown(
            account_id,
            "CodeArts statistics response has no credit total metric",
        );
    };

    let remaining = number(total.get("package_credit_remain"));
    let total_amount = number(total.get("package_credit_amount"));
    if remaining.is_none() && total_amount.is_none() {
        return QuotaSnapshot::unknown(
            account_id,
            "CodeArts credit total metric has no numeric amount",
        );
    }

    let end_date = payload
        .get("end_date")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut packages = Vec::new();
    for item in metrics {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() || name == TOTAL_METRIC {
            continue;
        }
        let amount = number(item.get("package_credit_amount"));
        let used = number(item.get("package_credit_used"));
        if amount.is_none() && used.is_none() {
            continue;
        }
        packages.push(json!({
            "name": name,
            "total": amount,
            "used": used,
            "end": end_date,
            "expiring": number(item.get("package_credit_expiring_amount")),
        }));
    }

    QuotaSnapshot {
        account_id: account_id.to_string(),
        group_id: None,
        model: None,
        unit: QuotaUnit::Credits,
        total: total_amount,
        remaining,
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: now,
        raw: Some(json!({
            "end_date": end_date,
            "packages": packages,
        })),
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    value.as_f64().map(|value| value.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_metric_wins_and_packages_are_kept() {
        let payload = json!({
            "end_date": "2026-11-02",
            "metrics": [
                {"name": "usageTotalPackageCredit", "package_credit_amount": 30000, "package_credit_used": 12000, "package_credit_remain": 18000},
                {"name": "usageBasicPackageCredit", "package_credit_amount": 20000, "package_credit_used": 5000, "package_credit_expiring_amount": 1000},
                {"name": "usageBonusPackageCredit", "package_credit_amount": "bad"}
            ]
        });
        let quota = parse_quota("acct", &payload, Utc::now());
        assert_eq!(quota.status, QuotaStatus::Known);
        assert_eq!(quota.unit, QuotaUnit::Credits);
        assert_eq!(quota.total, Some(30_000.0));
        assert_eq!(quota.remaining, Some(18_000.0));
        let raw = quota.raw.expect("raw");
        assert_eq!(raw["end_date"], "2026-11-02");
        assert_eq!(raw["packages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn missing_total_is_unknown_not_zero() {
        let payload =
            json!({"metrics": [{"name": "usageBasicPackageCredit", "package_credit_amount": 10}]});
        let quota = parse_quota("acct", &payload, Utc::now());
        assert_eq!(quota.status, QuotaStatus::Unknown);
        assert_eq!(quota.remaining, None);
        assert_eq!(quota.total, None);
    }

    #[test]
    fn numeric_strings_are_not_invented_as_amounts() {
        let payload = json!({"metrics": [{"name": "usageTotalPackageCredit", "package_credit_amount": "30000", "package_credit_remain": true}]});
        let quota = parse_quota("acct", &payload, Utc::now());
        assert_eq!(quota.status, QuotaStatus::Unknown);
    }
}
