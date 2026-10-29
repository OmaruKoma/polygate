//! `GET /api/v2/quota/usage` parsing.
//!
//! Source: Coding2API `src/provider/qoder/events.py:511` (`parse_quota`). The
//! reference sums `userQuota` and `addOnQuota` and treats a missing field as
//! zero. This project is stricter: a missing or non-numeric `remaining`/`total`
//! on the base package yields `unknown` with no numbers at all, because a zero
//! balance and an unreported balance are different facts.
//!
//! An absent `addOnQuota` object is read as "this account has no add-on package"
//! and contributes zero; a present-but-malformed one makes the snapshot unknown.

use serde_json::Value;

use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

/// Parse a quota payload. Never invents a balance.
pub fn parse_quota(account_id: &str, payload: &Value) -> QuotaSnapshot {
    let base = payload.get("userQuota").and_then(Value::as_object);
    let addon = payload.get("addOnQuota");

    let Some(base) = base else {
        return QuotaSnapshot::unknown(account_id, "Qoder quota response has no userQuota object");
    };

    let Some(base_total) = number(base.get("total")) else {
        return QuotaSnapshot::unknown(account_id, "Qoder userQuota.total is missing");
    };
    let Some(base_remaining) = number(base.get("remaining")) else {
        return QuotaSnapshot::unknown(account_id, "Qoder userQuota.remaining is missing");
    };

    let (addon_total, addon_remaining) = match addon {
        None | Some(Value::Null) => (0.0, 0.0),
        Some(value) => {
            let Some(object) = value.as_object() else {
                return QuotaSnapshot::unknown(account_id, "Qoder addOnQuota is not an object");
            };
            let Some(total) = number(object.get("total")) else {
                return QuotaSnapshot::unknown(account_id, "Qoder addOnQuota.total is missing");
            };
            let Some(remaining) = number(object.get("remaining")) else {
                return QuotaSnapshot::unknown(account_id, "Qoder addOnQuota.remaining is missing");
            };
            (total, remaining)
        }
    };

    QuotaSnapshot {
        account_id: account_id.to_string(),
        group_id: None,
        model: None,
        unit: QuotaUnit::Credits,
        total: Some(base_total + addon_total),
        remaining: Some((base_remaining + addon_remaining).max(0.0)),
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: chrono::Utc::now(),
        raw: Some(payload.clone()),
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_and_addon_are_summed() {
        let payload = json!({
            "userQuota": {"total": 100.0, "used": 40.0, "remaining": 60.0},
            "addOnQuota": {"total": 20.0, "used": 5.0, "remaining": 15.0},
            "expiresAt": 1_760_000_000_000_i64
        });
        let quota = parse_quota("a1", &payload);
        assert_eq!(quota.status, QuotaStatus::Known);
        assert_eq!(quota.unit, QuotaUnit::Credits);
        assert_eq!(quota.total, Some(120.0));
        assert_eq!(quota.remaining, Some(75.0));
        assert!(quota.error.is_none());
    }

    #[test]
    fn absent_addon_counts_as_zero() {
        let payload = json!({"userQuota": {"total": 50, "remaining": 50}});
        let quota = parse_quota("a1", &payload);
        assert_eq!(quota.status, QuotaStatus::Known);
        assert_eq!(quota.total, Some(50.0));
        assert_eq!(quota.remaining, Some(50.0));
    }

    #[test]
    fn missing_remaining_is_unknown_not_zero() {
        let payload = json!({"userQuota": {"total": 50}});
        let quota = parse_quota("a1", &payload);
        assert_eq!(quota.status, QuotaStatus::Unknown);
        assert_eq!(quota.total, None);
        assert_eq!(quota.remaining, None);
        assert!(quota.error.is_some());
    }

    #[test]
    fn malformed_addon_is_unknown() {
        let payload = json!({
            "userQuota": {"total": 50, "remaining": 50},
            "addOnQuota": {"total": "many"}
        });
        let quota = parse_quota("a1", &payload);
        assert_eq!(quota.status, QuotaStatus::Unknown);
        assert_eq!(quota.remaining, None);
    }

    #[test]
    fn missing_user_quota_is_unknown() {
        let quota = parse_quota("a1", &json!({"message": "ok"}));
        assert_eq!(quota.status, QuotaStatus::Unknown);
        assert_eq!(quota.remaining, None);
    }

    #[test]
    fn negative_remaining_is_clamped_to_zero_after_sum() {
        let payload = json!({
            "userQuota": {"total": 10, "remaining": -2},
            "addOnQuota": {"total": 0, "remaining": 0}
        });
        let quota = parse_quota("a1", &payload);
        assert_eq!(quota.remaining, Some(0.0));
        assert_eq!(quota.status, QuotaStatus::Known);
    }
}
