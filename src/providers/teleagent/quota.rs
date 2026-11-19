//! TeleAgent points accounting.
//!
//! Evidence: `docs/TELEAGENT_MIGRATION.md` §3. `GET
//! /superCowork/sapi/user/portal/companyStore/userCenter` answers
//! `{"code":200,"data":{…}}` with:
//!
//! * `current_points_balance` — a **string** (`"0.00"`), the spendable balance;
//! * `plan_display_name` — `免费版` / a paid plan name;
//! * `max_concurrent_tasks`, `end_date` and portal URLs, none of which are a
//!   quota total.
//!
//! There is no "granted total" in this payload, so `total` stays `None`. A
//! missing balance is reported as `unknown`, never as 0.

use chrono::Utc;
use serde_json::Value;

use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

pub fn parse_user_center(account_id: &str, value: &Value) -> QuotaSnapshot {
    let data = value.get("data").unwrap_or(value);
    let balance = data
        .get("current_points_balance")
        .and_then(|raw| match raw {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse::<f64>().ok(),
            _ => None,
        });
    let plan = data
        .get("plan_display_name")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match balance {
        Some(remaining) => QuotaSnapshot {
            account_id: account_id.to_string(),
            group_id: None,
            model: None,
            unit: QuotaUnit::Credits,
            total: None,
            remaining: Some(remaining),
            status: QuotaStatus::Known,
            error: None,
            refreshed_at: Utc::now(),
            raw: Some(value.clone()),
        },
        None => {
            let reason = if plan.is_empty() {
                "TeleAgent 用户中心没有返回积分余额".to_string()
            } else {
                format!("TeleAgent 用户中心没有返回积分余额（当前套餐：{plan}）")
            };
            let mut snapshot = QuotaSnapshot::unknown(account_id, reason);
            snapshot.raw = Some(value.clone());
            snapshot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_string_balance_becomes_a_known_credit_balance() {
        let snapshot = parse_user_center(
            "teleagent:cn:abc",
            &json!({
                "code": 200,
                "data": {
                    "current_points_balance": "123.45",
                    "plan_display_name": "免费版",
                    "max_concurrent_tasks": 3
                }
            }),
        );
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.remaining, Some(123.45));
        assert_eq!(snapshot.total, None);
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
    }

    #[test]
    fn a_missing_balance_stays_unknown_not_zero() {
        let snapshot = parse_user_center(
            "teleagent:cn:abc",
            &json!({"code": 200, "data": {"plan_display_name": "免费版"}}),
        );
        assert_eq!(snapshot.status, QuotaStatus::Unknown);
        assert!(snapshot.remaining.is_none());
        assert!(snapshot.error.unwrap().contains("免费版"));
    }
}
