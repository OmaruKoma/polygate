//! SQLite persistence.
//!
//! The repositories in this module know nothing about HTTP, providers or the
//! scheduler. They own a cloned connection pool; callers decide when to
//! migrate, when to encrypt credentials and when to close the database.
//!
//! Connections are configured for a small VPS: WAL journal, a bounded pool,
//! `busy_timeout` so concurrent writers wait instead of failing immediately,
//! and foreign keys enforced on every pooled connection.

mod accounts;
mod admin;
mod checkin;
mod credentials;
mod logs;
mod model_routes;
mod quota;
mod sessions;
mod settings;
mod usage;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::accounts::{Account, NewAccount};
use crate::error::GatewayError;
use crate::security::SecretBox;

pub use accounts::AccountRepo;
pub use admin::{AdminRepo, AdminSessionRow, AdminUserRow, AuthFlowRow};
pub use checkin::{CheckinRepo, RunClaim};
pub use credentials::{CredentialInfo, CredentialRepo};
pub use logs::{LogSummary, NewRequestLog, RequestLog, RequestLogRepo};
pub use model_routes::{ModelRoutePatch, ModelRouteRepo, ModelRouteRow, NewModelRoute};
pub use quota::QuotaRepo;
pub use sessions::SessionRepo;
pub use settings::SettingsRepo;
pub use usage::{NewUsageEvent, UsageEvent, UsageRepo, UsageSummary};

/// SQLite allows one writer at a time; more connections only add memory.
const MAX_POOL_CONNECTIONS: u32 = 8;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    /// Open (creating if needed) a database file, apply pending migrations and
    /// return a ready store. `max_connections` is clamped to `1..=8`.
    pub async fn open_file(
        path: impl AsRef<Path>,
        max_connections: u32,
    ) -> Result<Self, GatewayError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    GatewayError::config(format!(
                        "cannot create database directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }
        let options = base_options().filename(path).create_if_missing(true);
        let store = Self::connect(options, max_connections).await?;
        store.migrate().await?;
        Ok(store)
    }

    /// Open a database from a `sqlite://` URL (as used by config).
    pub async fn open_url(url: &str, max_connections: u32) -> Result<Self, GatewayError> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|e| GatewayError::config(format!("invalid database url: {e}")))?;
        let options = options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true)
            .create_if_missing(true);
        let store = Self::connect(options, max_connections).await?;
        store.migrate().await?;
        Ok(store)
    }

    /// Single-connection in-memory database, used by tests. Data disappears
    /// when the last connection is closed.
    pub async fn open_memory() -> Result<Self, GatewayError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(|e| GatewayError::config(format!("invalid database url: {e}")))?
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .acquire_timeout(BUSY_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(storage_error)?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn connect(
        options: SqliteConnectOptions,
        max_connections: u32,
    ) -> Result<Self, GatewayError> {
        let max_connections = max_connections.clamp(1, MAX_POOL_CONNECTIONS);
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .min_connections(1)
            .acquire_timeout(BUSY_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(storage_error)?;
        Ok(Self { pool })
    }

    /// Apply pending versioned migrations. Safe to call repeatedly: sqlx
    /// records applied versions in `_sqlx_migrations`.
    pub async fn migrate(&self) -> Result<(), GatewayError> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| {
                GatewayError::storage("database migration failed").with_detail(e.to_string())
            })?;
        Ok(())
    }

    /// Create an account and its encrypted credential in one transaction.
    /// Either both rows exist afterwards or neither does.
    pub async fn create_account(
        &self,
        secret_box: &SecretBox,
        new: &NewAccount,
    ) -> Result<Account, GatewayError> {
        let id = Uuid::new_v4().to_string();
        let mut plaintext = serde_json::to_vec(&new.credential)
            .map_err(|_| GatewayError::internal("credential serialization failed"))?;
        let payload = secret_box.encrypt_for_account(&id, &plaintext);
        plaintext.zeroize();
        let payload = payload?;

        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        accounts::insert_row(&mut *tx, &id, new, Utc::now())
            .await
            .map_err(storage_error)?;
        credentials::upsert_payload(&mut *tx, &id, &payload, Utc::now())
            .await
            .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;

        self.accounts()
            .get(&id)
            .await?
            .ok_or_else(|| GatewayError::internal("account disappeared after creation"))
    }

    /// Stop accepting work and close every pooled connection.
    pub async fn close(&self) {
        self.pool.close().await;
    }
    /// Whether the connection pool has been closed (diagnostics/tests).
    pub fn is_closed(&self) -> bool {
        self.pool.is_closed()
    }

    pub fn accounts(&self) -> AccountRepo {
        AccountRepo::new(self.pool.clone())
    }

    pub fn admin(&self) -> AdminRepo {
        AdminRepo::new(self.pool.clone())
    }

    pub fn credentials(&self) -> CredentialRepo {
        CredentialRepo::new(self.pool.clone())
    }

    pub fn checkin(&self) -> CheckinRepo {
        CheckinRepo::new(self.pool.clone())
    }

    pub fn sessions(&self) -> SessionRepo {
        SessionRepo::new(self.pool.clone())
    }

    pub fn quota(&self) -> QuotaRepo {
        QuotaRepo::new(self.pool.clone())
    }

    pub fn request_logs(&self) -> RequestLogRepo {
        RequestLogRepo::new(self.pool.clone())
    }

    pub fn model_routes(&self) -> ModelRouteRepo {
        ModelRouteRepo::new(self.pool.clone())
    }

    pub fn usage(&self) -> UsageRepo {
        UsageRepo::new(self.pool.clone())
    }

    pub fn settings(&self) -> SettingsRepo {
        SettingsRepo::new(self.pool.clone())
    }
}

fn base_options() -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true)
}

/// Classify a sqlx failure without leaking bound values. Constraint errors
/// carry only the constraint/table name in their message.
pub(crate) fn storage_error(err: sqlx::Error) -> GatewayError {
    if let Some(db) = err.as_database_error() {
        let message = if db.is_unique_violation() {
            "duplicate key"
        } else if db.is_foreign_key_violation() {
            "foreign key constraint failed"
        } else if db
            .code()
            .map(|code| code == "5" || code == "6")
            .unwrap_or(false)
            || db.message().contains("database is locked")
            || db.message().contains("database is busy")
        {
            "database is busy"
        } else {
            "database error"
        };
        let error = GatewayError::storage(message).with_detail(err.to_string());
        error.log("storage");
        return error;
    }
    GatewayError::from(err)
}

pub(crate) fn encode_ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn decode_ts(raw: &str) -> Result<DateTime<Utc>, GatewayError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|_| GatewayError::storage("invalid timestamp stored in database"))
}

pub(crate) fn limit(limit: i64) -> i64 {
    limit.clamp(1, 1000)
}
