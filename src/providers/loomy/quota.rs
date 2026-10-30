//! Loomy points accounting.
//!
//! Evidence: `docs/LOOMY_MIGRATION.md` §5. `GET /points/records` returns
//! `data.{balance, dailyBalance, availableBalance}`:
//!
//! * `balance` — permanent points (promotions, redemptions, leftovers);
//! * `dailyBalance` — the free daily grant, consumed first and reset each day;
//! * `availableBalance` — what the next request may spend (`balance + daily`).
//!
//! The snapshot keeps `availableBalance` as `remaining` and `balance + daily`
//! as `total`, both in `credits`. A missing number stays `None` — never 0.

use chrono::Utc;
use serde_json::Value;

use crate::error::{ErrorKind, GatewayError};
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};

pub fn parse_points(account_id: &str, value: &Value) -> Result<QuotaSnapshot, GatewayError> {
    let data = value.get("data").unwrap_or(value);
    let balance = number(data, "balance");
    let daily = number(data, "dailyBalance");
    let available = number(data, "availableBalance");
    let total = match (balance, daily) {
        (Some(balance), Some(daily)) => Some(balance + daily),
        (Some(balance), None) => Some(balance),
        (None, Some(daily)) => Some(daily),
        (None, None) => None,
    };
    if available.is_none() && total.is_none() {
        return Err(GatewayError::new(
            ErrorKind::UpstreamServer,
            "Loomy points payload has no balance fields",
        ));
    }
    Ok(QuotaSnapshot {
        account_id: account_id.to_string(),
        group_id: None,
        model: None,
        unit: QuotaUnit::Credits,
        total,
        remaining: available.or(total),
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: Utc::now(),
        raw: Some(data.clone()),
    })
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|raw| match raw {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn available_balance_is_remaining_and_the_grants_sum_to_total() {
        let snapshot = parse_points(
            "acc",
            &json!({"data": {"balance": 5500, "dailyBalance": 4976, "availableBalance": 10476}}),
        )
        .expect("parse");
        assert_eq!(snapshot.remaining, Some(10476.0));
        assert_eq!(snapshot.total, Some(10476.0));
        assert_eq!(snapshot.unit, QuotaUnit::Credits);
        assert_eq!(snapshot.status, QuotaStatus::Known);
    }

    #[test]
    fn numeric_strings_are_accepted() {
        let snapshot = parse_points(
            "acc",
            &json!({"data": {"balance": "10", "availableBalance": "12"}}),
        )
        .expect("parse");
        assert_eq!(snapshot.remaining, Some(12.0));
        assert_eq!(snapshot.total, Some(10.0));
    }

    #[test]
    fn a_payload_without_numbers_is_an_error_not_zero() {
        let error = parse_points("acc", &json!({"data": {"records": []}})).expect_err("empty");
        assert_eq!(error.kind, ErrorKind::UpstreamServer);
    }
}
