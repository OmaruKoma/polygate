//! WorkBuddy credit balance parsing.
//!
//! Evidence: `docs/WORKBUDDY_MIGRATION.md` §11.1; Coding2API
//! `src/provider/codebuddy/client.py:295-375` (MIT, protocol facts only):
//!
//! ```text
//! POST {billing_base}/v2/billing/meter/get-user-resource
//! data.Response.Data.Accounts[]            (older payloads nest one level less)
//!   Status == 0                            active package
//!   CapacityType == 4                      monthly cycle package
//!     CycleCapacitySize / CycleCapacityRemain / CycleEndTime
//!   otherwise                              one-off package
//!     CapacitySize / CapacityRemain / ExpiredTime
//! ```
//!
//! `*Precise` fields win and are returned as strings. The response-level
//! `TotalDosage` is **not** used: it reports the previous cycle and does not
//! move in real time (documented by the reference after measuring three
//! accounts). Remaining is the sum of per-package remainders.
//!
//! Unknown stays unknown: an empty or unusable package list produces
//! [`QuotaStatus::Unknown`], never `0` and never infinity.

use chrono::{NaiveDateTime, TimeZone, Utc};
use serde_json::{json, Value};

use crate::error::GatewayError;
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

const CAPACITY_MONTHLY: i64 = 4;
const RANGE_END: &str = "2099-12-31 23:59:59";

/// Request body for `get-user-resource`. No `ProductCode`: the reference
/// documents that some accounts reject it, and omitting it returns every
/// package (which is what the totals are built from).
pub fn quota_request_body(now: i64) -> Value {
    let begin = Utc
        .timestamp_opt(now, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    json!({
        "PageNumber": 1,
        "PageSize": 200,
        "Status": [0, 3],
        "PackageEndTimeRangeBegin": begin,
        "PackageEndTimeRangeEnd": RANGE_END,
    })
}

pub fn parse_quota(payload: &Value, now: i64) -> Result<QuotaSnapshot, GatewayError> {
    let data = payload
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| GatewayError::upstream_server("WorkBuddy quota has no data object"))?;
    if let Some(response) = data.get("Response").and_then(Value::as_object) {
        if let Some(error) = response.get("Error").and_then(Value::as_object) {
            if let Some(code) = error.get("Code").filter(|code| !is_blank(code)) {
                let message = error
                    .get("Message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Err(GatewayError::upstream_server(format!(
                    "WorkBuddy quota was rejected: {code} {message}"
                ))
                .with_detail(message.to_string()));
            }
        }
    }
    let accounts = accounts_of(data).ok_or_else(|| {
        GatewayError::upstream_server("WorkBuddy quota response has no accounts list")
    })?;

    let mut monthly = Bucket::default();
    let mut one_off = Bucket::default();
    let mut packages = Vec::new();
    for account in accounts {
        let Some(account) = account.as_object() else {
            continue;
        };
        if account.get("Status").and_then(Value::as_i64) != Some(0) {
            continue;
        }
        let is_monthly =
            account.get("CapacityType").and_then(Value::as_i64) == Some(CAPACITY_MONTHLY);
        // Cycle fields are the current-period numbers; a one-off package may only
        // carry the plain `Capacity*` pair. Try the natural pair first and fall
        // back so a package is never skipped just because the other vocabulary
        // was used.
        let (size_fields, remain_fields, end_field) = if is_monthly {
            (
                ["CycleCapacitySize", "CapacitySize"],
                ["CycleCapacityRemain", "CapacityRemain"],
                "CycleEndTime",
            )
        } else {
            (
                ["CapacitySize", "CycleCapacitySize"],
                ["CapacityRemain", "CycleCapacityRemain"],
                "ExpiredTime",
            )
        };
        let total = first_capacity(account, &size_fields);
        if total <= 0.0 {
            continue;
        }
        let remaining = first_capacity(account, &remain_fields).clamp(0.0, total);
        let end = account.get(end_field).and_then(epoch_of);
        let bucket = if is_monthly {
            &mut monthly
        } else {
            &mut one_off
        };
        bucket.total += total;
        bucket.remaining += remaining;
        bucket.end = match (bucket.end, end) {
            (Some(current), Some(next)) => Some(current.min(next)),
            (current, next) => current.or(next),
        };
        packages.push(json!({
            "kind": if is_monthly { "monthly" } else { "one_off" },
            "name": account.get("PackageName").and_then(Value::as_str).unwrap_or_default(),
            "total": total,
            "remaining": remaining,
            "used": (total - remaining).max(0.0),
            "end_at": end,
        }));
    }

    let total = monthly.total + one_off.total;
    if total <= 0.0 {
        // The account may legitimately have no active package; that is not a
        // transport failure, but it is also not "0 credits" (a package may be
        // created later). Keep it explicitly unknown.
        let mut snapshot =
            QuotaSnapshot::unknown("", "WorkBuddy returned no active credit package");
        snapshot.unit = QuotaUnit::Credits;
        snapshot.raw = Some(json!({
            "source": "workbuddy",
            "packages": [],
        }));
        return Ok(snapshot);
    }
    let remaining = (monthly.remaining + one_off.remaining).clamp(0.0, total);
    let mut snapshot = QuotaSnapshot::unknown("", "");
    snapshot.unit = QuotaUnit::Credits;
    snapshot.status = QuotaStatus::Known;
    snapshot.total = Some(total);
    snapshot.remaining = Some(remaining);
    snapshot.error = None;
    snapshot.raw = Some(json!({
        "source": "workbuddy",
        "probed_at": now,
        "monthly": {
            "total": monthly.total,
            "remaining": monthly.remaining,
            "used": (monthly.total - monthly.remaining).max(0.0),
            "cycle_end": monthly.end,
        },
        "one_off": {
            "total": one_off.total,
            "remaining": one_off.remaining,
            "used": (one_off.total - one_off.remaining).max(0.0),
            "expires_at": one_off.end,
        },
        "packages": packages,
    }));
    Ok(snapshot)
}

#[derive(Default)]
struct Bucket {
    total: f64,
    remaining: f64,
    end: Option<i64>,
}

fn accounts_of(data: &serde_json::Map<String, Value>) -> Option<&Vec<Value>> {
    if let Some(list) = data
        .get("Response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("Data"))
        .and_then(Value::as_object)
        .and_then(|inner| inner.get("Accounts"))
        .and_then(Value::as_array)
    {
        return Some(list);
    }
    for key in ["Accounts", "accounts"] {
        if let Some(list) = data.get(key).and_then(Value::as_array) {
            return Some(list);
        }
    }
    data.get("Response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("Accounts"))
        .and_then(Value::as_array)
}

/// First present field wins; a `*Precise` string wins inside one field.
fn first_capacity(account: &serde_json::Map<String, Value>, fields: &[&str]) -> f64 {
    fields
        .iter()
        .find_map(|field| capacity(account, field))
        .unwrap_or(0.0)
}

/// `*Precise` wins; the upstream sends it as a numeric string.
fn capacity(account: &serde_json::Map<String, Value>, field: &str) -> Option<f64> {
    let precise = account.get(&format!("{field}Precise"));
    number(precise).or_else(|| number(account.get(field)))
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

/// Epoch seconds from a number (seconds or milliseconds) or
/// `YYYY-MM-DD HH:MM:SS`. The upstream timezone is unverified, so the string
/// form is read as UTC; the raw value is also kept in the snapshot.
fn epoch_of(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(if number > 100_000_000_000 {
            number / 1000
        } else {
            number
        });
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = text.parse::<i64>() {
        return Some(if parsed > 100_000_000_000 {
            parsed / 1000
        } else {
            parsed
        });
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|naive| Utc.from_local_datetime(&naive).single())
        .map(|datetime| datetime.timestamp())
}

fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Number(number) => number.as_i64() == Some(0),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(accounts: Value) -> Value {
        json!({"data": {"Response": {"Data": {"Accounts": accounts}}}})
    }

    #[test]
    fn monthly_and_one_off_packages_are_separated() {
        let payload = payload(json!([
            {
                "Status": 0,
                "CapacityType": 4,
                "CycleCapacitySize": 100,
                "CycleCapacityRemainPrecise": "40",
                "CycleEndTime": "2099-01-01 00:00:00",
                "PackageName": "monthly"
            },
            {
                "Status": 0,
                "CapacityType": 1,
                "CapacitySize": "50",
                "CapacityRemain": 20,
                "ExpiredTime": 4_000_000_000_i64,
                "PackageName": "one-off"
            },
            {"Status": 2, "CapacitySize": 999, "CapacityRemain": 999}
        ]));
        let snapshot = parse_quota(&payload, 1_000).unwrap();
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
        assert_eq!(snapshot.total, Some(150.0));
        assert_eq!(snapshot.remaining, Some(60.0));
        let raw = snapshot.raw.unwrap();
        assert_eq!(raw["monthly"]["remaining"], 40.0);
        assert_eq!(raw["one_off"]["remaining"], 20.0);
        assert_eq!(raw["packages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn precise_strings_are_parsed() {
        let payload = payload(json!([{
            "Status": 0,
            "CapacitySize": 1,
            "CapacitySizePrecise": "500",
            "CapacityRemainPrecise": "250.5"
        }]));
        let snapshot = parse_quota(&payload, 1_000).unwrap();
        assert_eq!(snapshot.total, Some(500.0));
        assert_eq!(snapshot.remaining, Some(250.5));
    }

    #[test]
    fn no_active_package_is_unknown_not_zero() {
        let snapshot = parse_quota(&payload(json!([])), 1_000).unwrap();
        assert_eq!(snapshot.status, QuotaStatus::Unknown);
        assert_eq!(snapshot.total, None);
        assert_eq!(snapshot.remaining, None);
    }

    #[test]
    fn a_business_error_is_a_failure_not_an_empty_balance() {
        let error = parse_quota(
            &json!({"data": {"Response": {"Error": {"Code": "InvalidParameter", "Message": "bad"}}}}),
            1_000,
        )
        .unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }

    #[test]
    fn a_missing_accounts_list_is_a_failure() {
        let error = parse_quota(&json!({"data": {}}), 1_000).unwrap_err();
        assert_eq!(error.kind, crate::error::ErrorKind::UpstreamServer);
    }
}
