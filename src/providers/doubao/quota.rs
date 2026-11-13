//! Doubao subscription/quota probe.
//!
//! Evidence (2026-11-13): `POST /alice/commerce/sale/subscription/quota/summary/`
//! with `{"product_line":"doubao"}` answers `code: 0` and needs no `a_bogus`
//! signature, unlike the other `/alice/*` endpoints. The payload carries the
//! member state, entitlement groups and per-window limits; a free account
//! returns empty groups, so the balance stays `unknown` rather than `0`.
//!
//! The upstream never reports token counts for a chat, so nothing here is
//! derived from characters — a missing number stays `None`.

use chrono::Utc;
use serde_json::Value;

use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

pub fn parse_summary(account_id: &str, value: &Value) -> QuotaSnapshot {
    let data = value.get("data").unwrap_or(&Value::Null);
    let has_subscription = data
        .pointer("/member_info/hasActiveSubscription")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exhausted = data
        .pointer("/enterprise_window_limit_section/usage_exhausted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let limit = data
        .pointer("/window_limit_section/window_limit_groups")
        .and_then(Value::as_array)
        .and_then(|groups| groups.first());

    let remaining = limit
        .and_then(|group| group.get("remaining"))
        .and_then(Value::as_f64);
    let total = limit
        .and_then(|group| group.get("total"))
        .and_then(Value::as_f64);

    if remaining.is_none() && total.is_none() {
        let reason = if has_subscription {
            "豆包账号有订阅，但上游没有下发额度窗口数值"
        } else {
            "豆包账号没有订阅，上游没有返回任何额度窗口数值（余额未知）"
        };
        let mut snapshot = QuotaSnapshot::unknown(account_id, reason);
        if exhausted {
            snapshot.status = QuotaStatus::Unknown;
        }
        snapshot.raw = Some(value.clone());
        return snapshot;
    }

    QuotaSnapshot {
        account_id: account_id.to_string(),
        group_id: None,
        model: None,
        unit: QuotaUnit::Credits,
        total,
        remaining,
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: Utc::now(),
        raw: Some(value.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_free_account_is_reported_as_unknown_not_zero() {
        let snapshot = parse_summary(
            "doubao:cn:abc",
            &json!({
                "code": 0,
                "data": {
                    "member_info": {"hasActiveSubscription": false},
                    "enterprise_window_limit_section": {"entitlement_count": 0, "usage_exhausted": false},
                    "window_limit_section": {}
                }
            }),
        );
        assert_eq!(snapshot.status, QuotaStatus::Unknown);
        assert!(snapshot.remaining.is_none());
        assert!(snapshot.error.is_some());
        assert!(snapshot.raw.is_some());
    }

    #[test]
    fn a_window_limit_becomes_a_known_balance() {
        let snapshot = parse_summary(
            "doubao:cn:abc",
            &json!({
                "data": {
                    "member_info": {"hasActiveSubscription": true},
                    "window_limit_section": {
                        "window_limit_groups": [{"total": 1000, "remaining": 640}]
                    }
                }
            }),
        );
        assert_eq!(snapshot.status, QuotaStatus::Known);
        assert_eq!(snapshot.remaining, Some(640.0));
        assert_eq!(snapshot.total, Some(1000.0));
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
    }
}
