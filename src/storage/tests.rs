use chrono::{Duration, Utc};

use super::*;
use crate::accounts::{Account, Credential, NewAccount};
use crate::config::{ProviderKind, Region};
use crate::error::ErrorKind;
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};
use crate::security::SecretBox;
use crate::sessions::{Session, SessionState};

fn master_key(byte: u8) -> SecretBox {
    SecretBox::from_master_key(&[byte; 32]).expect("32-byte key")
}

fn new_account(name: &str) -> NewAccount {
    NewAccount {
        provider: "mock-cn".into(),
        kind: ProviderKind::Mock,
        region: Some(Region::Cn),
        name: name.into(),
        credential: serde_json::json!({
            "token": "super-secret-token",
            "refresh_token": "refresh-123"
        }),
        external_id: None,
        weight: 2,
        max_concurrency: 3,
        quota_group: None,
        enabled: true,
        metadata: serde_json::json!({"note": "test"}),
    }
}

fn new_session(client_id: &str, account_id: &str) -> Session {
    let now = Utc::now();
    Session {
        id: String::new(),
        client_session_id: client_id.into(),
        provider: "mock-cn".into(),
        kind: ProviderKind::Mock,
        region: "cn".into(),
        account_id: account_id.into(),
        upstream_session_id: None,
        state: SessionState::Active,
        binding_revision: 0,
        invalid_reason: None,
        created_at: now,
        last_used_at: now,
        expires_at: now + Duration::hours(1),
        request_count: 0,
        metadata: serde_json::json!({}),
    }
}

async fn test_store() -> (tempfile::TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polygate.db");
    let store = Storage::open_file(&path, 4).await.unwrap();
    (dir, store)
}

async fn insert_account(store: &Storage, name: &str) -> Account {
    store.accounts().insert(&new_account(name)).await.unwrap()
}

#[tokio::test]
async fn empty_database_migrates_and_creates_required_tables() {
    let (_dir, store) = test_store().await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(&store.pool)
    .await
    .unwrap();
    for required in [
        "accounts",
        "admin_auth_flows",
        "admin_sessions",
        "admin_users",
        "checkin_runs",
        "checkin_schedules",
        "credential_refresh_leases",
        "credentials",
        "model_routes",
        "quota_snapshots",
        "request_logs",
        "schema_version",
        "sessions",
        "settings",
        "usage_events",
    ] {
        assert!(
            tables.iter().any(|table| table == required),
            "missing table {required}"
        );
    }

    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM schema_version")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(version, 8);

    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(journal.to_ascii_lowercase(), "wal");

    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
}

#[tokio::test]
async fn corrupt_database_file_fails_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.db");
    std::fs::write(&path, b"this is not a sqlite database").unwrap();

    let err = match Storage::open_file(&path, 2).await {
        Ok(_) => panic!("a corrupt database must not open"),
        Err(err) => err,
    };
    assert_eq!(err.kind, ErrorKind::Storage);
    assert!(!err.message.is_empty());
}

#[tokio::test]
async fn migration_is_idempotent() {
    let (_dir, store) = test_store().await;
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();

    let schema_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_version")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(schema_rows, 8);
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(applied, 8);
}

#[tokio::test]
async fn account_crud_roundtrip() {
    let (_dir, store) = test_store().await;
    let accounts = store.accounts();

    let created = accounts.insert(&new_account("primary")).await.unwrap();
    assert_eq!(created.provider, "mock-cn");
    assert_eq!(created.kind, ProviderKind::Mock);
    assert_eq!(created.region, Region::Cn);
    assert_eq!(created.name, "primary");
    assert_eq!(created.weight, 2);
    assert_eq!(created.max_concurrency, 3);
    assert!(created.enabled);
    assert_eq!(created.metadata, serde_json::json!({"note": "test"}));
    assert_eq!(created.auth_status, crate::accounts::AuthStatus::Unknown);
    assert!(created.credential_hint.contains("token=***"));
    assert!(!created.credential_hint.contains("super-secret-token"));

    let loaded = accounts.get(&created.id).await.unwrap().unwrap();
    assert_eq!(loaded.id, created.id);
    assert_eq!(accounts.list().await.unwrap().len(), 1);

    let mut updated = loaded.clone();
    updated.name = "renamed".into();
    updated.weight = 5;
    updated.enabled = false;
    updated.last_error = Some("probe failed".into());
    assert!(accounts.update(&updated).await.unwrap());

    let after = accounts.get(&created.id).await.unwrap().unwrap();
    assert_eq!(after.name, "renamed");
    assert_eq!(after.weight, 5);
    assert!(!after.enabled);
    assert_eq!(after.last_error.as_deref(), Some("probe failed"));

    assert!(accounts.set_enabled(&created.id, true).await.unwrap());
    assert!(accounts
        .update_auth(&created.id, crate::accounts::AuthStatus::Valid, None)
        .await
        .unwrap());
    let after = accounts.get(&created.id).await.unwrap().unwrap();
    assert!(after.enabled);
    assert_eq!(after.auth_status, crate::accounts::AuthStatus::Valid);
    assert!(after.auth_checked_at.is_some());

    assert!(accounts.delete(&created.id).await.unwrap());
    assert!(accounts.get(&created.id).await.unwrap().is_none());
    assert!(accounts.list().await.unwrap().is_empty());
    assert!(!accounts.delete(&created.id).await.unwrap());
}

#[tokio::test]
async fn credential_is_encrypted_at_rest_and_reads_back() {
    let (_dir, store) = test_store().await;
    let secret_box = master_key(7);
    let account = insert_account(&store, "with-credential").await;
    let credential = Credential::from_value(serde_json::json!({
        "token": "super-secret-token",
        "cookie": "session=sid-abc",
    }));

    store
        .credentials()
        .save(&secret_box, &account.id, &credential)
        .await
        .unwrap();

    let raw = store
        .credentials()
        .raw_payload(&account.id)
        .await
        .unwrap()
        .expect("payload stored");
    assert!(raw.starts_with("v1:"));
    assert!(!raw.contains("super-secret-token"));
    assert!(!raw.contains("sid-abc"));

    let loaded = store
        .credentials()
        .load(&secret_box, &account.id)
        .await
        .unwrap()
        .expect("credential exists");
    assert_eq!(loaded.to_json(), credential.to_json());
    assert!(store
        .credentials()
        .load(&secret_box, "missing-account")
        .await
        .unwrap()
        .is_none());

    let replacement = Credential::from_value(serde_json::json!({"token": "second-token"}));
    store
        .credentials()
        .save(&secret_box, &account.id, &replacement)
        .await
        .unwrap();
    let loaded = store
        .credentials()
        .load(&secret_box, &account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.to_json(), replacement.to_json());
}

#[tokio::test]
async fn wrong_master_key_cannot_decrypt_credentials() {
    let (_dir, store) = test_store().await;
    let writer = master_key(1);
    let reader = master_key(2);
    let account = insert_account(&store, "wrong-key").await;
    let credential = Credential::from_value(serde_json::json!({"token": "super-secret-token"}));

    store
        .credentials()
        .save(&writer, &account.id, &credential)
        .await
        .unwrap();

    let err = store
        .credentials()
        .load(&reader, &account.id)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Internal);
    assert!(!err.message.contains("super-secret-token"));
    assert!(!err.message.contains("v1:"));
}

#[tokio::test]
async fn credential_for_unknown_account_is_rejected_by_foreign_key() {
    let (_dir, store) = test_store().await;
    let err = store
        .credentials()
        .save(
            &master_key(7),
            "does-not-exist",
            &Credential::from_value(serde_json::json!({"token": "x"})),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Storage);
    assert!(err.message.contains("foreign key"));
}

#[tokio::test]
async fn session_requires_existing_account() {
    let (_dir, store) = test_store().await;
    let err = store
        .sessions()
        .save(&new_session("client-1", "missing-account"))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Storage);
    assert!(err.message.contains("foreign key"));
    assert!(store
        .sessions()
        .get("client-1", ProviderKind::Mock, Region::Cn)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn session_binding_is_upserted_by_client_provider_region() {
    let (_dir, store) = test_store().await;
    let first = insert_account(&store, "first").await;
    let second = insert_account(&store, "second").await;

    let stored = store
        .sessions()
        .save(&new_session("client-1", &first.id))
        .await
        .unwrap();
    let mut moved = stored.clone();
    moved.account_id = second.id.clone();
    moved.upstream_session_id = Some("upstream-9".into());
    let updated = store.sessions().save(&moved).await.unwrap();

    assert_eq!(updated.id, stored.id);
    assert_eq!(updated.account_id, second.id);
    assert_eq!(updated.upstream_session_id.as_deref(), Some("upstream-9"));
    assert_eq!(
        store
            .sessions()
            .list_by_account(&second.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .sessions()
        .list_by_account(&first.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .sessions()
            .count_active_by_account(&second.id)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn quota_unknown_stays_unknown() {
    let (_dir, store) = test_store().await;
    let account = insert_account(&store, "quota-unknown").await;
    let snapshot = QuotaSnapshot::unknown(&account.id, "provider does not expose a balance");

    store.quota().save(&snapshot).await.unwrap();
    let loaded = store
        .quota()
        .latest_for_account(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.status, QuotaStatus::Unknown);
    assert_eq!(loaded.unit, QuotaUnit::Unknown);
    assert!(loaded.total.is_none());
    assert!(loaded.remaining.is_none());
    assert_eq!(
        loaded.error.as_deref(),
        Some("provider does not expose a balance")
    );

    let (used, limit): (Option<f64>, Option<f64>) =
        sqlx::query_as("SELECT used, limit_value FROM quota_snapshots WHERE account_id = ?")
            .bind(&account.id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert!(used.is_none());
    assert!(limit.is_none());
}

#[tokio::test]
async fn quota_known_snapshot_roundtrips_used_and_limit() {
    let (_dir, store) = test_store().await;
    let account = insert_account(&store, "quota-known").await;
    let snapshot = QuotaSnapshot {
        account_id: account.id.clone(),
        group_id: Some("team".into()),
        model: None,
        unit: QuotaUnit::Credits,
        total: Some(100.0),
        remaining: Some(40.0),
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: Utc::now(),
        raw: Some(serde_json::json!({"source": "mock"})),
    };

    store.quota().save(&snapshot).await.unwrap();
    let loaded = store
        .quota()
        .latest_for_account(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.status, QuotaStatus::Known);
    assert_eq!(loaded.unit, QuotaUnit::Credits);
    assert_eq!(loaded.total, Some(100.0));
    assert_eq!(loaded.remaining, Some(40.0));
    assert_eq!(loaded.group_id.as_deref(), Some("team"));
    assert_eq!(loaded.raw, snapshot.raw);
}

#[tokio::test]
async fn shared_group_returns_latest_row_per_account_without_summing() {
    let (_dir, store) = test_store().await;
    let first = insert_account(&store, "shared-a").await;
    let second = insert_account(&store, "shared-b").await;
    let now = Utc::now();

    for (account, remaining) in [(&first, 50.0), (&second, 50.0)] {
        let snapshot = QuotaSnapshot {
            account_id: account.id.clone(),
            group_id: Some("shared".into()),
            model: None,
            unit: QuotaUnit::Credits,
            total: Some(100.0),
            remaining: Some(remaining),
            status: QuotaStatus::Known,
            error: None,
            refreshed_at: now,
            raw: None,
        };
        store.quota().save(&snapshot).await.unwrap();
    }

    let rows = store.quota().latest_for_group("shared").await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].account_id, rows[1].account_id);
    assert!(rows.iter().all(|row| row.remaining == Some(50.0)));

    let newer = QuotaSnapshot {
        account_id: first.id.clone(),
        group_id: Some("shared".into()),
        model: None,
        unit: QuotaUnit::Credits,
        total: Some(100.0),
        remaining: Some(10.0),
        status: QuotaStatus::Known,
        error: None,
        refreshed_at: now + Duration::seconds(1),
        raw: None,
    };
    store.quota().save(&newer).await.unwrap();
    let rows = store.quota().latest_for_group("shared").await.unwrap();
    assert_eq!(rows.len(), 2);
    let first_row = rows
        .iter()
        .find(|row| row.account_id == first.id)
        .expect("first account row");
    assert_eq!(first_row.remaining, Some(10.0));
}

#[tokio::test]
async fn usage_events_count_requests_and_tokens_separately() {
    let (_dir, store) = test_store().await;
    let account = insert_account(&store, "usage").await;

    let base = NewUsageEvent {
        request_id: "req-1".into(),
        provider_kind: "mock".into(),
        account_id: Some(account.id.clone()),
        model: Some("mock-chat".into()),
        input_tokens: 10,
        output_tokens: 5,
        status: "ok".into(),
    };
    store.usage().record(&base).await.unwrap();
    let mut second = base.clone();
    second.request_id = "req-2".into();
    second.input_tokens = 20;
    second.output_tokens = 7;
    store.usage().record(&second).await.unwrap();

    let summary = store.usage().summary(Some(&account.id)).await.unwrap();
    assert_eq!(summary.requests, 2);
    assert_eq!(summary.input_tokens, 30);
    assert_eq!(summary.output_tokens, 12);
    assert_eq!(store.usage().list_recent(10).await.unwrap().len(), 2);

    let sessions = store
        .sessions()
        .count_active_by_account(&account.id)
        .await
        .unwrap();
    assert_eq!(sessions, 0, "session assignments are counted separately");
}

#[tokio::test]
async fn duplicate_request_id_is_a_storage_error() {
    let (_dir, store) = test_store().await;
    let event = NewUsageEvent {
        request_id: "duplicate-request".into(),
        provider_kind: "mock".into(),
        account_id: None,
        model: None,
        input_tokens: 1,
        output_tokens: 1,
        status: "ok".into(),
    };
    store.usage().record(&event).await.unwrap();
    let err = store.usage().record(&event).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Storage);
    assert_eq!(err.message, "duplicate key");
    assert!(!err.message.contains("duplicate-request"));
}

#[tokio::test]
async fn settings_roundtrip_and_sensitive_keys_are_rejected() {
    let (_dir, store) = test_store().await;
    let settings = store.settings();

    settings.set("ui.theme", "dark").await.unwrap();
    settings.set("ui.theme", "light").await.unwrap();
    assert_eq!(
        settings.get("ui.theme").await.unwrap().as_deref(),
        Some("light")
    );
    settings
        .set("scheduler.strategy", "round_robin")
        .await
        .unwrap();
    assert_eq!(settings.list().await.unwrap().len(), 2);
    assert!(settings.delete("scheduler.strategy").await.unwrap());

    for key in [
        "master_key",
        "upstream.api_token",
        "admin_password",
        "provider_credential",
    ] {
        let err = settings.set(key, "do-not-store").await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadRequest, "key {key}");
        assert!(settings.get(key).await.unwrap().is_none());
    }
    assert_eq!(settings.list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn account_and_credential_are_created_in_one_transaction() {
    let (_dir, store) = test_store().await;
    let secret_box = master_key(5);
    let account = store
        .create_account(&secret_box, &new_account("transactional"))
        .await
        .unwrap();

    let credential = store
        .credentials()
        .load(&secret_box, &account.id)
        .await
        .unwrap()
        .expect("credential committed with the account");
    assert_eq!(credential.get("token"), Some("super-secret-token"));
    assert_eq!(store.accounts().list().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transactional_creations_commit_complete_rows() {
    let (_dir, store) = test_store().await;
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let secret_box = master_key(6);
            store
                .create_account(&secret_box, &new_account(&format!("tx-{i}")))
                .await
                .unwrap()
        }));
    }
    let mut created = Vec::new();
    for handle in handles {
        created.push(handle.await.unwrap());
    }

    assert_eq!(store.accounts().list().await.unwrap().len(), 8);
    let secret_box = master_key(6);
    for account in created {
        let credential = store
            .credentials()
            .load(&secret_box, &account.id)
            .await
            .unwrap()
            .expect("every committed account has its credential");
        assert_eq!(credential.get("token"), Some("super-secret-token"));
    }
}

#[tokio::test]
async fn data_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polygate.db");
    let secret_box = master_key(3);
    let account = {
        let store = Storage::open_file(&path, 4).await.unwrap();
        let account = insert_account(&store, "persistent").await;
        store
            .credentials()
            .save(
                &secret_box,
                &account.id,
                &Credential::from_value(serde_json::json!({"token": "persisted"})),
            )
            .await
            .unwrap();
        store
            .sessions()
            .save(&new_session("client-persist", &account.id))
            .await
            .unwrap();
        store.settings().set("ui.theme", "dark").await.unwrap();
        store
            .usage()
            .record(&NewUsageEvent {
                request_id: "req-persist".into(),
                provider_kind: "mock".into(),
                account_id: Some(account.id.clone()),
                model: Some("mock-chat".into()),
                input_tokens: 3,
                output_tokens: 4,
                status: "ok".into(),
            })
            .await
            .unwrap();
        store
            .quota()
            .save(&QuotaSnapshot::unknown(&account.id, "no balance endpoint"))
            .await
            .unwrap();
        store.close().await;
        account
    };

    let reopened = Storage::open_file(&path, 4).await.unwrap();
    let loaded = reopened.accounts().get(&account.id).await.unwrap().unwrap();
    assert_eq!(loaded.name, "persistent");
    assert_eq!(
        reopened
            .credentials()
            .load(&secret_box, &account.id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("persisted")
    );
    assert!(reopened
        .sessions()
        .get("client-persist", ProviderKind::Mock, Region::Cn)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        reopened
            .settings()
            .get("ui.theme")
            .await
            .unwrap()
            .as_deref(),
        Some("dark")
    );
    let summary = reopened.usage().summary(None).await.unwrap();
    assert_eq!(summary.requests, 1);
    assert_eq!(summary.input_tokens, 3);
    assert_eq!(summary.output_tokens, 4);
    let quota = reopened
        .quota()
        .latest_for_account(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(quota.status, QuotaStatus::Unknown);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_session_updates_are_atomic() {
    let (_dir, store) = test_store().await;
    let account = insert_account(&store, "concurrent").await;
    let stored = store
        .sessions()
        .save(&new_session("client-concurrent", &account.id))
        .await
        .unwrap();
    let session_id = stored.id.clone();

    let tasks = 8;
    let per_task = 10;
    let mut handles = Vec::new();
    for _ in 0..tasks {
        let store = store.clone();
        let session_id = session_id.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..per_task {
                store.sessions().touch(&session_id, 600).await.unwrap();
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let loaded = store
        .sessions()
        .get_by_id(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.request_count,
        (tasks * per_task) as u64,
        "every atomic increment must be preserved"
    );
}

#[tokio::test]
async fn concurrent_setting_upserts_leave_one_consistent_value() {
    let (_dir, store) = test_store().await;
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .settings()
                .set("concurrent.key", &format!("value-{i}"))
                .await
                .unwrap();
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    let value = store.settings().get("concurrent.key").await.unwrap();
    assert!(value
        .as_deref()
        .is_some_and(|value| value.starts_with("value-")));
    assert_eq!(store.settings().list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn deleting_account_cascades_credentials_sessions_and_quota() {
    let (_dir, store) = test_store().await;
    let account = insert_account(&store, "cascade").await;
    store
        .credentials()
        .save(
            &master_key(4),
            &account.id,
            &Credential::from_value(serde_json::json!({"token": "x"})),
        )
        .await
        .unwrap();
    store
        .sessions()
        .save(&new_session("client-cascade", &account.id))
        .await
        .unwrap();
    store
        .quota()
        .save(&QuotaSnapshot::unknown(&account.id, "n/a"))
        .await
        .unwrap();

    assert!(store.accounts().delete(&account.id).await.unwrap());
    assert!(store
        .credentials()
        .raw_payload(&account.id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .sessions()
        .get("client-cascade", ProviderKind::Mock, Region::Cn)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .quota()
        .latest_for_account(&account.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn refresh_leases_coordinate_across_connections_and_expire() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polygate.db");
    let first = Storage::open_file(&path, 4).await.unwrap();
    let second = Storage::open_file(&path, 4).await.unwrap();
    let account = first
        .create_account(&master_key(1), &new_account("lease"))
        .await
        .unwrap();

    assert!(first
        .credentials()
        .acquire_refresh_lease(&account.id, "owner-a", 1_000, 60)
        .await
        .unwrap());
    assert!(
        !second
            .credentials()
            .acquire_refresh_lease(&account.id, "owner-b", 1_010, 60)
            .await
            .unwrap(),
        "a live lease must not be taken over"
    );
    // Releasing with the wrong owner does nothing.
    second
        .credentials()
        .release_refresh_lease(&account.id, "owner-b")
        .await
        .unwrap();
    assert!(!second
        .credentials()
        .acquire_refresh_lease(&account.id, "owner-b", 1_020, 60)
        .await
        .unwrap());
    // An expired lease may be taken over.
    assert!(second
        .credentials()
        .acquire_refresh_lease(&account.id, "owner-b", 1_070, 60)
        .await
        .unwrap());
    second
        .credentials()
        .release_refresh_lease(&account.id, "owner-b")
        .await
        .unwrap();
    assert!(first
        .credentials()
        .acquire_refresh_lease(&account.id, "owner-a", 1_071, 60)
        .await
        .unwrap());
}

#[tokio::test]
async fn request_log_summary_counts_failures_and_latency() {
    let (_dir, store) = test_store().await;
    let repo = store.request_logs();
    for (status, latency) in [(200u16, 10i64), (500, 30), (402, 20)] {
        repo.record(&super::NewRequestLog {
            request_id: format!("r-{status}"),
            provider: Some("mock-cn".into()),
            provider_kind: "mock".into(),
            region: Some("cn".into()),
            account_id: None,
            model_alias: Some("alias".into()),
            upstream_model: Some("mock-chat".into()),
            api: "openai.chat".into(),
            stream: false,
            http_status: status,
            error_category: None,
            input_tokens: 1,
            output_tokens: 2,
            latency_ms: latency,
        })
        .await
        .unwrap();
    }
    let summary = repo
        .summary_since(chrono::Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(summary.total, 3);
    assert_eq!(summary.failures, 2);
    assert_eq!(summary.avg_latency_ms, 20.0);
    let failures = repo.list_failures(10).await.unwrap();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0].http_status, 402);
}

#[tokio::test]
async fn empty_request_log_summary_reports_zero() {
    let (_dir, store) = test_store().await;
    let summary = store
        .request_logs()
        .summary_since(chrono::Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(summary.total, 0);
    assert_eq!(summary.failures, 0);
    assert_eq!(summary.avg_latency_ms, 0.0);
}
