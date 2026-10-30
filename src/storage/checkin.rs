//! Persistent check-in runs and schedules.
//!
//! The claim transition is the concurrency mechanism: `pending|failed -> running`
//! with a conditional `UPDATE ... WHERE id = ? AND status = ?`. SQLite serializes
//! writers, so exactly one worker observes `rows_affected == 1`; every other
//! caller sees the row in `running` and must not execute.
//!
//! Nothing here calls an upstream. This repository only records intent, ownership
//! and outcome, so a process crash leaves an auditable `running` row instead of
//! silently forgetting the day.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::checkin::{
    CheckinErrorCategory, CheckinPolicy, CheckinRecord, CheckinRetryMode, CheckinSchedule,
    CheckinStatus,
};
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;

use super::{decode_ts, encode_ts, storage_error};

const COLUMNS: &str = "id, account_id, provider_kind, region, business_date, timezone, status, \
     attempt_count, started_at, completed_at, last_error_category, result_summary, lease_until, \
     created_at, updated_at";

/// Result of trying to own one (account, business date) run.
#[derive(Debug, Clone)]
pub enum RunClaim {
    /// The caller now owns the run and must complete it.
    Claimed(CheckinRecord),
    /// Another worker owns a live lease.
    Busy(CheckinRecord),
    /// A previous worker died while running; the outcome must be confirmed (or
    /// marked unknown), never re-executed.
    Interrupted(CheckinRecord),
    /// Terminal row for this business date; return it unchanged.
    Existing(CheckinRecord),
}

#[derive(Clone)]
pub struct CheckinRepo {
    pool: SqlitePool,
}

impl CheckinRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_run(
        &self,
        account_id: &str,
        business_date: NaiveDate,
    ) -> Result<Option<CheckinRecord>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM checkin_runs WHERE account_id = ? AND business_date = ?"
        );
        let row = sqlx::query(&query)
            .bind(account_id)
            .bind(date_str(business_date))
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.map(|row| map_run(&row)).transpose()
    }

    /// Most recent runs for one account, newest business date first.
    pub async fn list_runs(
        &self,
        account_id: &str,
        limit: i64,
    ) -> Result<Vec<CheckinRecord>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM checkin_runs WHERE account_id = ? \
             ORDER BY business_date DESC LIMIT ?"
        );
        let rows = sqlx::query(&query)
            .bind(account_id)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_run).collect()
    }

    /// Runs owned by a worker whose lease has expired. Used only at startup and
    /// by explicit recovery; they are resolved via a status probe, not re-run.
    pub async fn list_stale_running(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<CheckinRecord>, GatewayError> {
        let query = format!(
            "SELECT {COLUMNS} FROM checkin_runs WHERE status = 'running' \
             AND (lease_until IS NULL OR lease_until <= ?) ORDER BY business_date"
        );
        let rows = sqlx::query(&query)
            .bind(encode_ts(now))
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter().map(map_run).collect()
    }

    /// Insert a `pending` row if today's run does not exist yet.
    ///
    /// Exposed for crash-recovery tests; normal callers go through [`Self::claim`].
    #[cfg(test)]
    pub(crate) async fn insert_pending(
        &self,
        account_id: &str,
        kind: ProviderKind,
        region: Region,
        business_date: NaiveDate,
        timezone: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<CheckinRecord>, GatewayError> {
        let id = Uuid::new_v4().to_string();
        let now_raw = encode_ts(now);
        let result = sqlx::query(
            "INSERT INTO checkin_runs (id, account_id, provider_kind, region, business_date, \
             timezone, status, attempt_count, result_summary, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'pending', 0, '', ?, ?) \
             ON CONFLICT(account_id, business_date) DO NOTHING",
        )
        .bind(&id)
        .bind(account_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(date_str(business_date))
        .bind(timezone)
        .bind(&now_raw)
        .bind(&now_raw)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_run(account_id, business_date).await
    }

    /// Create-or-claim one business date run.
    ///
    /// * `pending` rows are claimable by anyone;
    /// * `failed` rows only when `retry` allows (manual, or a retryable category
    ///   inside the automatic attempt budget);
    /// * `unsupported` rows only on an explicit manual retry: nothing was
    ///   claimed upstream, and the reason may have been configuration that has
    ///   since been fixed;
    /// * `running` rows are busy while the lease holds, otherwise `Interrupted`;
    /// * success / unknown rows are terminal and returned as-is.
    #[allow(clippy::too_many_arguments)]
    pub async fn claim(
        &self,
        account_id: &str,
        kind: ProviderKind,
        region: Region,
        business_date: NaiveDate,
        timezone: &str,
        now: DateTime<Utc>,
        lease: Duration,
        retry: CheckinRetryMode,
    ) -> Result<RunClaim, GatewayError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;

        let id = Uuid::new_v4().to_string();
        let now_raw = encode_ts(now);
        sqlx::query(
            "INSERT INTO checkin_runs (id, account_id, provider_kind, region, business_date, \
             timezone, status, attempt_count, result_summary, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'pending', 0, '', ?, ?) \
             ON CONFLICT(account_id, business_date) DO NOTHING",
        )
        .bind(&id)
        .bind(account_id)
        .bind(kind.as_str())
        .bind(region.as_str())
        .bind(date_str(business_date))
        .bind(timezone)
        .bind(&now_raw)
        .bind(&now_raw)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;

        let query = format!(
            "SELECT {COLUMNS} FROM checkin_runs WHERE account_id = ? AND business_date = ?"
        );
        let row = sqlx::query(&query)
            .bind(account_id)
            .bind(date_str(business_date))
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_error)?;
        let record = map_run(&row)?;

        let claimable = match record.status {
            CheckinStatus::Pending => true,
            CheckinStatus::Failed => retry.allows_failed(&record),
            CheckinStatus::Unsupported => retry.is_manual(),
            _ => false,
        };

        if claimable {
            let lease_until = encode_ts(now + lease);
            let result = sqlx::query(
                "UPDATE checkin_runs SET status = 'running', attempt_count = attempt_count + 1, \
                 started_at = ?, completed_at = NULL, lease_until = ?, \
                 last_error_category = NULL, result_summary = '', updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(&now_raw)
            .bind(&lease_until)
            .bind(&now_raw)
            .bind(&record.id)
            .bind(record.status.as_str())
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
            if result.rows_affected() == 1 {
                let row = sqlx::query(&format!("SELECT {COLUMNS} FROM checkin_runs WHERE id = ?"))
                    .bind(&record.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(storage_error)?;
                let claimed = map_run(&row)?;
                tx.commit().await.map_err(storage_error)?;
                return Ok(RunClaim::Claimed(claimed));
            }
        }

        tx.commit().await.map_err(storage_error)?;
        Ok(match record.status {
            CheckinStatus::Running => {
                if record.lease_until.is_some_and(|until| until > now) {
                    RunClaim::Busy(record)
                } else {
                    RunClaim::Interrupted(record)
                }
            }
            _ => RunClaim::Existing(record),
        })
    }

    /// Resolve an `unknown` row after an upstream status query confirmed a
    /// success. Only `unknown -> succeeded|already_checked_in` is allowed; an
    /// unconfirmed or failed probe must leave the row `unknown` (no blind retry).
    pub async fn confirm_unknown(
        &self,
        id: &str,
        status: CheckinStatus,
        summary: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, GatewayError> {
        if !status.is_success() {
            return Err(GatewayError::bad_request(
                "only a successful status can confirm an unknown run",
            ));
        }
        let result = sqlx::query(
            "UPDATE checkin_runs SET status = ?, completed_at = ?, last_error_category = NULL, \
             result_summary = ?, updated_at = ? WHERE id = ? AND status = 'unknown'",
        )
        .bind(status.as_str())
        .bind(encode_ts(now))
        .bind(summary)
        .bind(encode_ts(now))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Finish a claimed run. Only a `running` row can be completed, so a worker
    /// that lost its lease cannot overwrite a newer outcome.
    pub async fn complete(
        &self,
        id: &str,
        status: CheckinStatus,
        category: Option<CheckinErrorCategory>,
        summary: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE checkin_runs SET status = ?, completed_at = ?, last_error_category = ?, \
             result_summary = ?, lease_until = NULL, updated_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(status.as_str())
        .bind(encode_ts(now))
        .bind(category.map(|value| value.as_str()))
        .bind(summary)
        .bind(encode_ts(now))
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    // ---------------------------------------------------------------- schedules

    pub async fn upsert_schedule(
        &self,
        schedule: &CheckinSchedule,
        now: DateTime<Utc>,
    ) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO checkin_schedules (account_id, enabled, timezone, local_time, \
             jitter_secs, catch_up_secs, max_attempts, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(account_id) DO UPDATE SET enabled = excluded.enabled, \
             timezone = excluded.timezone, local_time = excluded.local_time, \
             jitter_secs = excluded.jitter_secs, catch_up_secs = excluded.catch_up_secs, \
             max_attempts = excluded.max_attempts, updated_at = excluded.updated_at",
        )
        .bind(&schedule.account_id)
        .bind(if schedule.policy.enabled { 1i64 } else { 0 })
        .bind(&schedule.policy.timezone)
        .bind(&schedule.policy.local_time)
        .bind(schedule.policy.jitter_secs as i64)
        .bind(schedule.policy.catch_up_secs as i64)
        .bind(i64::from(schedule.policy.max_attempts))
        .bind(encode_ts(now))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn get_schedule(
        &self,
        account_id: &str,
    ) -> Result<Option<CheckinSchedule>, GatewayError> {
        let row = sqlx::query(
            "SELECT account_id, enabled, timezone, local_time, jitter_secs, catch_up_secs, \
             max_attempts FROM checkin_schedules WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(|row| map_schedule(&row)).transpose()
    }

    pub async fn list_schedules(&self) -> Result<Vec<CheckinSchedule>, GatewayError> {
        let rows = sqlx::query(
            "SELECT account_id, enabled, timezone, local_time, jitter_secs, catch_up_secs, \
             max_attempts FROM checkin_schedules ORDER BY account_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(map_schedule).collect()
    }

    pub async fn set_schedule_enabled(
        &self,
        account_id: &str,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<bool, GatewayError> {
        let result = sqlx::query(
            "UPDATE checkin_schedules SET enabled = ?, updated_at = ? WHERE account_id = ?",
        )
        .bind(if enabled { 1i64 } else { 0 })
        .bind(encode_ts(now))
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_schedule(&self, account_id: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM checkin_schedules WHERE account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(result.rows_affected() == 1)
    }
}

fn date_str(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn map_run(row: &sqlx::sqlite::SqliteRow) -> Result<CheckinRecord, GatewayError> {
    let kind_raw: String = row.try_get("provider_kind")?;
    let kind = ProviderKind::from_db_str(&kind_raw)
        .ok_or_else(|| GatewayError::storage("unknown provider kind stored in checkin_runs"))?;
    let region_raw: String = row.try_get("region")?;
    let region = Region::from_db_str(&region_raw)
        .ok_or_else(|| GatewayError::storage("unknown region stored in checkin_runs"))?;
    let date_raw: String = row.try_get("business_date")?;
    let business_date = NaiveDate::parse_from_str(&date_raw, "%Y-%m-%d")
        .map_err(|_| GatewayError::storage("invalid business_date stored in checkin_runs"))?;
    let status_raw: String = row.try_get("status")?;
    let started_raw: Option<String> = row.try_get("started_at")?;
    let completed_raw: Option<String> = row.try_get("completed_at")?;
    let lease_raw: Option<String> = row.try_get("lease_until")?;
    let category_raw: Option<String> = row.try_get("last_error_category")?;
    let created_raw: String = row.try_get("created_at")?;
    let updated_raw: String = row.try_get("updated_at")?;

    Ok(CheckinRecord {
        id: row.try_get("id")?,
        account_id: row.try_get("account_id")?,
        kind,
        region,
        business_date,
        timezone: row.try_get("timezone")?,
        status: CheckinStatus::from_db_str(&status_raw),
        attempt_count: row.try_get::<i64, _>("attempt_count")? as u32,
        started_at: started_raw.as_deref().map(decode_ts).transpose()?,
        completed_at: completed_raw.as_deref().map(decode_ts).transpose()?,
        error_category: category_raw
            .as_deref()
            .and_then(CheckinErrorCategory::from_db_str),
        summary: row.try_get("result_summary")?,
        lease_until: lease_raw.as_deref().map(decode_ts).transpose()?,
        created_at: decode_ts(&created_raw)?,
        updated_at: decode_ts(&updated_raw)?,
    })
}

fn map_schedule(row: &sqlx::sqlite::SqliteRow) -> Result<CheckinSchedule, GatewayError> {
    Ok(CheckinSchedule {
        account_id: row.try_get("account_id")?,
        policy: CheckinPolicy {
            enabled: row.try_get::<i64, _>("enabled")? != 0,
            timezone: row.try_get("timezone")?,
            local_time: row.try_get("local_time")?,
            jitter_secs: row.try_get::<i64, _>("jitter_secs")? as u64,
            catch_up_secs: row.try_get::<i64, _>("catch_up_secs")? as u64,
            max_attempts: row.try_get::<i64, _>("max_attempts")? as u32,
        },
    })
}
