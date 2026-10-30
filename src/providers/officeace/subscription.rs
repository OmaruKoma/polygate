//! `GET /v1/subscription` → points balance and the daily bonus activity.
//!
//! Points are the main ledger here (unlike CodeArts, which has a separate token
//! pool), so `remaining`/`total` are points with no unit conversion. Two sources
//! add up, matching the official front end:
//!
//! * `skus[].quotas[]` entries whose `sku_attr_code` is `officeace_points`;
//! * `bonus_skus[]` (new-user grant plus the daily check-in).
//!
//! Expired entries are excluded. A missing number stays `None`; it is never
//! turned into 0.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Utc};
use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

use super::region;

/// Today's check-in activity, when the upstream lists one.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyActivity {
    pub activity_id: String,
    pub activity_name: String,
    /// The grant was already claimed on today's Beijing date.
    pub claimed: bool,
    /// Points granted by the activity (total − used).
    pub credit: f64,
}

pub fn parse_snapshot(
    account_id: &str,
    value: &Value,
    now: DateTime<Utc>,
) -> Result<QuotaSnapshot, GatewayError> {
    let moment = now.timestamp();
    let mut total = 0.0;
    let mut used = 0.0;
    let mut saw_points = false;

    for sku in array(value, "skus") {
        for quota in array(sku, "quotas") {
            if quota.get("sku_attr_code").and_then(Value::as_str) != Some(region::POINTS_SKU) {
                continue;
            }
            let end = epoch(quota.get("expired_time"));
            if end.is_some_and(|end| end <= moment) {
                continue;
            }
            saw_points = true;
            total += number(quota.get("sku_value"));
            used += number(quota.get("current_value"));
        }
    }
    for bonus in array(value, "bonus_skus") {
        let end = epoch(bonus.get("expired_time"));
        if end.is_some_and(|end| end <= moment) {
            continue;
        }
        saw_points = true;
        total += number(bonus.get("points"));
        used += number(bonus.get("current_value"));
    }

    if !saw_points {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "OfficeAce subscription response has no points entries",
        ));
    }
    let remaining = (total - used).max(0.0);
    Ok(QuotaSnapshot {
        account_id: account_id.to_string(),
        group_id: None,
        model: None,
        unit: QuotaUnit::Credits,
        total: Some(total),
        remaining: Some(remaining),
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: now,
        raw: Some(value.clone()),
    })
}

/// The daily check-in activity. The upstream replaces it after each day, so an
/// entry that has not expired yet and became effective today is exactly the
/// "already checked in today" signal.
pub fn daily_activity(value: &Value, now: DateTime<Utc>) -> Option<DailyActivity> {
    let moment = now.timestamp();
    let today = beijing_day(moment);
    for bonus in array(value, "bonus_skus") {
        let resource = bonus
            .get("cbc_resource_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let name = bonus
            .get("activity_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !resource.contains("daily") && !name.contains("签到") && !name.contains("每日") {
            continue;
        }
        if epoch(bonus.get("expired_time")).is_some_and(|end| end <= moment) {
            continue;
        }
        let claimed = epoch(bonus.get("effective_time"))
            .map(|effective| beijing_day(effective) == today)
            .unwrap_or(false);
        return Some(DailyActivity {
            activity_id: bonus
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            activity_name: if name.is_empty() {
                "每日签到".into()
            } else {
                name.to_string()
            },
            claimed,
            credit: (number(bonus.get("points")) - number(bonus.get("current_value"))).max(0.0),
        });
    }
    None
}

/// `{activity_id: remaining points}` for before/after comparison.
///
/// Gains are measured as the **remaining** delta: `points` is the cumulative
/// amount granted across desktop check-ins and manual claims, so using it as
/// "what this run earned" would overstate the reward.
pub fn bonus_remaining(value: &Value) -> BTreeMap<String, f64> {
    let mut result = BTreeMap::new();
    for bonus in array(value, "bonus_skus") {
        let activity = bonus
            .get("activity_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if activity.is_empty() {
            continue;
        }
        result.insert(
            activity.to_string(),
            (number(bonus.get("points")) - number(bonus.get("current_value"))).max(0.0),
        );
    }
    result
}

/// Next Beijing midnight (epoch seconds). The daily activity expires at
/// 23:59:59 UTC+8 and is replaced at 00:00, which is when a new claim becomes
/// possible.
pub fn next_beijing_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let offset = FixedOffset::east_opt(region::BEIJING_OFFSET_SECONDS as i32)
        .expect("Beijing is a valid offset");
    let shifted = now.with_timezone(&offset);
    let day_start = offset
        .with_ymd_and_hms(shifted.year(), shifted.month(), shifted.day(), 0, 0, 0)
        .single()
        .unwrap_or(shifted);
    (day_start + chrono::Duration::days(1)).with_timezone(&Utc)
}

fn beijing_day(epoch: i64) -> String {
    let offset = FixedOffset::east_opt(region::BEIJING_OFFSET_SECONDS as i32)
        .expect("Beijing is a valid offset");
    DateTime::from_timestamp(epoch, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch"))
        .with_timezone(&offset)
        .format("%Y-%m-%d")
        .to_string()
}

fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter())
        .into_iter()
        .flatten()
        .filter(|item| item.is_object())
}

fn number(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn epoch(value: Option<&Value>) -> Option<i64> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|value| value.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-30T04:00:00Z")
            .expect("date")
            .with_timezone(&Utc)
    }

    fn subscription() -> Value {
        json!({
            "skus": [{"quotas": [
                {"sku_attr_code": "officeace_points", "sku_value": "4000",
                 "current_value": "1000", "expired_time": "2026-12-31T15:59:59Z"},
                {"sku_attr_code": "officeace_points", "sku_value": "100",
                 "current_value": "50", "expired_time": "2020-01-01T00:00:00Z"}
            ]}],
            "bonus_skus": [
                {"activity_id": "bonus-daily-1", "activity_name": "每日签到",
                 "cbc_resource_id": "bonus:daily", "points": "1000",
                 "current_value": "200", "effective_time": "2026-10-30T01:00:00Z",
                 "expired_time": "2026-10-30T15:59:59Z"}
            ]
        })
    }

    #[test]
    fn expired_entries_are_excluded_and_points_are_the_balance() {
        let snapshot = parse_snapshot("acc", &subscription(), now()).expect("snapshot");
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
        assert_eq!(snapshot.total, Some(5000.0));
        assert_eq!(snapshot.remaining, Some(3800.0));
        assert_eq!(snapshot.status, QuotaStatus::Known);
    }

    #[test]
    fn todays_daily_entry_marks_the_day_as_claimed() {
        let daily = daily_activity(&subscription(), now()).expect("activity");
        assert!(daily.claimed);
        assert_eq!(daily.credit, 800.0);
        assert_eq!(daily.activity_name, "每日签到");
    }

    #[test]
    fn a_daily_entry_that_became_effective_on_another_day_is_not_a_claim_today() {
        let value = json!({"bonus_skus": [
            {"activity_id": "bonus-daily-1", "activity_name": "每日签到",
             "cbc_resource_id": "bonus:daily", "points": "1000",
             "current_value": "0", "effective_time": "2026-10-29T01:00:00Z",
             "expired_time": "2026-11-02T15:59:59Z"}
        ]});
        let daily = daily_activity(&value, now()).expect("activity");
        assert!(!daily.claimed);
        let expired = daily_activity(&subscription(), now() + chrono::Duration::days(2));
        assert!(expired.is_none(), "expired activities are ignored");
    }

    #[test]
    fn gains_are_measured_on_the_remaining_amount() {
        let before = bonus_remaining(&json!({"bonus_skus": [
            {"activity_id": "a", "points": "1000", "current_value": "200"}
        ]}));
        let after = bonus_remaining(&subscription());
        let gained: f64 = after
            .iter()
            .map(|(key, value)| (value - before.get(key).copied().unwrap_or(0.0)).max(0.0))
            .sum();
        assert_eq!(gained, 800.0);
    }

    #[test]
    fn a_payload_without_points_is_a_protocol_error() {
        let error = parse_snapshot("acc", &json!({"skus": []}), now()).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }

    #[test]
    fn the_next_beijing_midnight_is_16_utc() {
        let next = next_beijing_midnight(now());
        assert_eq!(next.to_rfc3339(), "2026-10-30T16:00:00+00:00");
    }
}
