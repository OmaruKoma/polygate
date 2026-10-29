use std::sync::Arc;

use chrono::{Duration, Utc};

use super::{AccountService, AccountUpdate, CreateAccount};
use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::security::SecretBox;
use crate::sessions::{Session, SessionState};
use crate::storage::Storage;

fn secret_box(byte: u8) -> Arc<SecretBox> {
    Arc::new(SecretBox::from_master_key(&[byte; 32]).expect("32-byte key"))
}

async fn memory_store() -> Storage {
    Storage::open_memory().await.expect("memory database")
}

fn service(store: &Storage, byte: u8) -> AccountService {
    AccountService::new(store.clone(), secret_box(byte))
}

fn credential(token: &str) -> serde_json::Value {
    serde_json::json!({
        "token": token,
        "refresh_token": "refresh-secret",
        "user_id": "user-1"
    })
}

fn request(name: &str, credential: Option<serde_json::Value>) -> CreateAccount {
    CreateAccount::new(ProviderKind::Mock, Region::Cn, name, credential)
}

fn session(client_id: &str, account_id: &str) -> Session {
    let now = Utc::now();
    Session {
        id: String::new(),
        client_session_id: client_id.into(),
        provider: "mock-cn".into(),
        kind: ProviderKind::Mock,
        region: Region::Cn.as_str().into(),
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

#[tokio::test]
async fn creates_one_account_with_encrypted_credential() {
    let store = memory_store().await;
    let accounts = service(&store, 1);
    let account = accounts
        .create(request("primary", Some(credential("token-a"))))
        .await
        .unwrap();

    assert!(!account.id.is_empty());
    assert_eq!(account.kind, ProviderKind::Mock);
    assert_eq!(account.region, Region::Cn);
    assert_eq!(account.auth_status, AuthStatus::Unknown);
    assert!(account.enabled);
    assert!(account.credential_hint.contains("token=***"));
    assert!(!account.credential_hint.contains("token-a"));

    let loaded = accounts.get(&account.id).await.unwrap().unwrap();
    assert_eq!(loaded.id, account.id);
    assert_eq!(accounts.list().await.unwrap().len(), 1);
    assert_eq!(
        accounts
            .authorization_context(&account.id)
            .await
            .unwrap()
            .credential
            .unwrap()
            .get("token"),
        Some("token-a")
    );
}

#[tokio::test]
async fn two_accounts_for_the_same_provider_coexist() {
    let store = memory_store().await;
    let accounts = service(&store, 1);
    let first = accounts
        .create(request("first", Some(credential("token-a"))))
        .await
        .unwrap();
    let second = accounts
        .create(request("second", Some(credential("token-b"))))
        .await
        .unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(accounts.list().await.unwrap().len(), 2);
    assert_eq!(
        accounts
            .list_by_provider(ProviderKind::Mock, Region::Cn)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn cn_and_international_accounts_are_never_mixed() {
    let store = memory_store().await;
    let accounts = service(&store, 1);
    let cn = accounts
        .create(CreateAccount::new(
            ProviderKind::Codearts,
            Region::Cn,
            "cn",
            Some(credential("cn")),
        ))
        .await
        .unwrap();
    let intl = accounts
        .create(CreateAccount::new(
            ProviderKind::Codearts,
            Region::Intl,
            "intl",
            Some(credential("intl")),
        ))
        .await
        .unwrap();

    assert_eq!(
        accounts
            .list_available(ProviderKind::Codearts, Region::Cn)
            .await
            .unwrap()
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        vec![cn.id.as_str()]
    );
    assert_eq!(
        accounts
            .list_available(ProviderKind::Codearts, Region::Intl)
            .await
            .unwrap()
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        vec![intl.id.as_str()]
    );

    let err = accounts
        .authorization_context_for(&cn.id, ProviderKind::Codearts, Region::Intl)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::BadRequest);
}

#[tokio::test]
async fn disabling_keeps_the_account_and_its_credential() {
    let store = memory_store().await;
    let accounts = service(&store, 1);
    let account = accounts
        .create(request("toggle", Some(credential("token-a"))))
        .await
        .unwrap();

    assert!(accounts.set_enabled(&account.id, false).await.unwrap());
    assert!(!accounts.is_enabled(&account.id).await.unwrap().unwrap());
    assert!(!accounts.get(&account.id).await.unwrap().unwrap().enabled);
    let status = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert!(status.present);
    assert_eq!(
        accounts
            .credentials()
            .load(&account.id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("token-a")
    );
    // Disabling does not alter auth status.
    assert_eq!(
        accounts.auth_status(&account.id).await.unwrap(),
        Some(AuthStatus::Unknown)
    );

    assert!(accounts.set_enabled(&account.id, true).await.unwrap());
    assert!(accounts.is_enabled(&account.id).await.unwrap().unwrap());
}

#[tokio::test]
async fn credentials_are_encrypted_at_rest() {
    let store = memory_store().await;
    let accounts = service(&store, 7);
    let account = accounts
        .create(request("encrypted", Some(credential("super-secret-token"))))
        .await
        .unwrap();

    let raw = store
        .credentials()
        .raw_payload(&account.id)
        .await
        .unwrap()
        .expect("ciphertext");
    assert!(raw.starts_with("v1:"));
    assert!(!raw.contains("super-secret-token"));
    assert!(!raw.contains("refresh-secret"));

    let decrypted = accounts
        .credentials()
        .load(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decrypted.get("token"), Some("super-secret-token"));
}

#[tokio::test]
async fn wrong_master_key_cannot_decrypt_and_does_not_leak() {
    let store = memory_store().await;
    let writer = service(&store, 1);
    let account = writer
        .create(request("wrong-key", Some(credential("super-secret-token"))))
        .await
        .unwrap();

    let reader = service(&store, 2);
    let err = reader.credentials().load(&account.id).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Internal);
    assert!(!err.message.contains("super-secret-token"));
    assert!(!err.message.contains("v1:"));
}

#[tokio::test]
async fn account_aad_rejects_cross_account_decryption() {
    let store = memory_store().await;
    let key = secret_box(3);
    let accounts = AccountService::new(store.clone(), Arc::clone(&key));
    let first = accounts
        .create(request("first", Some(credential("first-secret"))))
        .await
        .unwrap();
    let second = accounts
        .create(request("second", Some(credential("second-secret"))))
        .await
        .unwrap();

    let raw = store
        .credentials()
        .raw_payload(&first.id)
        .await
        .unwrap()
        .unwrap();
    let err = key
        .decrypt_for_account(&second.id, &raw)
        .expect_err("AAD must bind to the source account");
    assert_eq!(err.kind, ErrorKind::Internal);
    assert!(!err.message.contains("first-secret"));
}

#[tokio::test]
async fn credential_update_uses_revision_and_records_time() {
    let store = memory_store().await;
    let accounts = service(&store, 4);
    let account = accounts
        .create(request("update", Some(credential("old-token"))))
        .await
        .unwrap();

    let before = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.revision, Some(0));
    assert!(before.updated_at.is_some());

    let updated = accounts
        .credentials()
        .update(
            &account.id,
            &Credential::from_value(credential("new-token")),
            before.revision.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.revision, Some(1));
    assert!(updated.updated_at >= before.updated_at);
    assert_eq!(
        accounts
            .credentials()
            .load(&account.id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("new-token")
    );

    let stale = accounts
        .credentials()
        .update(
            &account.id,
            &Credential::from_value(credential("stale-token")),
            before.revision.unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.kind, ErrorKind::BadRequest);
    assert_eq!(
        accounts
            .credentials()
            .load(&account.id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("new-token")
    );
}

#[tokio::test]
async fn credential_save_is_create_only_and_delete_keeps_account() {
    let store = memory_store().await;
    let accounts = service(&store, 5);
    let account = accounts
        .create(request("delete", Some(credential("original"))))
        .await
        .unwrap();

    let err = accounts
        .credentials()
        .save(
            &account.id,
            &Credential::from_value(credential("replacement")),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::BadRequest);
    assert_eq!(
        accounts
            .credentials()
            .load(&account.id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("original")
    );

    assert!(accounts.credentials().delete(&account.id).await.unwrap());
    assert!(!accounts.credentials().delete(&account.id).await.unwrap());
    let status = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!status.present);
    assert_eq!(status.revision, None);
    assert!(accounts.get(&account.id).await.unwrap().is_some());
    assert!(accounts
        .authorization_context(&account.id)
        .await
        .unwrap()
        .credential
        .is_none());

    let saved = accounts
        .credentials()
        .save(&account.id, &Credential::from_value(credential("again")))
        .await
        .unwrap();
    assert_eq!(saved.revision, Some(0));
    assert!(saved.present);
}

#[tokio::test]
async fn deleting_an_account_cascades_credentials_and_sessions() {
    let store = memory_store().await;
    let accounts = service(&store, 6);
    let account = accounts
        .create(request("cascade", Some(credential("token"))))
        .await
        .unwrap();
    store
        .sessions()
        .save(&session("client-1", &account.id))
        .await
        .unwrap();

    assert!(accounts.delete(&account.id).await.unwrap());
    assert!(accounts.get(&account.id).await.unwrap().is_none());
    assert!(store
        .credentials()
        .info(&account.id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .sessions()
        .list_by_account(&account.id)
        .await
        .unwrap()
        .is_empty());
    assert!(!accounts.delete(&account.id).await.unwrap());
}

#[tokio::test]
async fn auth_status_transitions_and_timeout_do_not_invalidate() {
    let store = memory_store().await;
    let accounts = service(&store, 8);
    let account = accounts
        .create(request("auth", Some(credential("token"))))
        .await
        .unwrap();
    assert_eq!(account.auth_status, AuthStatus::Unknown);

    assert_eq!(
        accounts
            .record_auth_status(&account.id, AuthStatus::Valid, None)
            .await
            .unwrap(),
        AuthStatus::Valid
    );
    assert_eq!(
        accounts
            .record_auth_status(&account.id, AuthStatus::Expired, Some("expired"))
            .await
            .unwrap(),
        AuthStatus::Expired
    );
    assert_eq!(
        accounts
            .record_auth_status(&account.id, AuthStatus::Invalid, Some("401"))
            .await
            .unwrap(),
        AuthStatus::Invalid
    );
    let invalid_transition = accounts
        .record_auth_status(&account.id, AuthStatus::Expired, None)
        .await
        .unwrap_err();
    assert_eq!(invalid_transition.kind, ErrorKind::BadRequest);
    assert_eq!(
        accounts
            .record_auth_status(&account.id, AuthStatus::Valid, None)
            .await
            .unwrap(),
        AuthStatus::Valid
    );

    let timeout = GatewayError::timeout("network timed out");
    assert_eq!(
        accounts
            .mark_auth_failure(&account.id, &timeout)
            .await
            .unwrap(),
        AuthStatus::Valid
    );
    assert_eq!(
        accounts.auth_status(&account.id).await.unwrap(),
        Some(AuthStatus::Valid)
    );

    let rejected = GatewayError::new(ErrorKind::AuthInvalid, "credential rejected");
    assert_eq!(
        accounts
            .mark_auth_failure(&account.id, &rejected)
            .await
            .unwrap(),
        AuthStatus::Invalid
    );
    assert_eq!(
        accounts
            .credentials()
            .status(&account.id)
            .await
            .unwrap()
            .unwrap()
            .auth_status,
        AuthStatus::Invalid
    );

    // Invalid credentials are excluded from dispatch but their secret remains.
    assert!(accounts
        .list_available(ProviderKind::Mock, Region::Cn)
        .await
        .unwrap()
        .is_empty());
    assert!(accounts
        .credentials()
        .load(&account.id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn credential_invalidation_is_explicit_and_keeps_secret() {
    let store = memory_store().await;
    let accounts = service(&store, 9);
    let account = accounts
        .create(request("invalidate", Some(credential("token"))))
        .await
        .unwrap();

    assert_eq!(
        accounts
            .credentials()
            .invalidate(&account.id, "operator marked it invalid")
            .await
            .unwrap(),
        AuthStatus::Invalid
    );
    assert_eq!(
        accounts.auth_status(&account.id).await.unwrap(),
        Some(AuthStatus::Invalid)
    );
    assert!(accounts
        .credentials()
        .load(&account.id)
        .await
        .unwrap()
        .is_some());
    assert!(accounts.is_enabled(&account.id).await.unwrap().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_credential_updates_have_one_winner() {
    let store = memory_store().await;
    let accounts = service(&store, 10);
    let account = accounts
        .create(request("race", Some(credential("initial"))))
        .await
        .unwrap();
    let revision = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap()
        .revision
        .unwrap();

    let left = Credential::from_value(credential("left"));
    let right = Credential::from_value(credential("right"));
    let (first, second) = tokio::join!(
        accounts.credentials().update(&account.id, &left, revision),
        accounts.credentials().update(&account.id, &right, revision)
    );

    assert_ne!(first.is_ok(), second.is_ok(), "exactly one writer must win");
    let status = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.revision, Some(1));
    let token = accounts
        .credentials()
        .load(&account.id)
        .await
        .unwrap()
        .unwrap()
        .get("token")
        .unwrap()
        .to_string();
    assert!(token == "left" || token == "right");
}

#[tokio::test]
async fn accounts_and_credentials_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polygate.db");
    let key = secret_box(11);
    let account_id = {
        let store = Storage::open_file(&path, 4).await.unwrap();
        let accounts = AccountService::new(store.clone(), Arc::clone(&key));
        let account = accounts
            .create(request("persistent", Some(credential("persisted"))))
            .await
            .unwrap();
        let id = account.id.clone();
        drop(accounts);
        store.close().await;
        id
    };

    let store = Storage::open_file(&path, 4).await.unwrap();
    let accounts = AccountService::new(store.clone(), key);
    let account = accounts.get(&account_id).await.unwrap().unwrap();
    assert_eq!(account.name, "persistent");
    assert_eq!(
        accounts
            .credentials()
            .load(&account_id)
            .await
            .unwrap()
            .unwrap()
            .get("token"),
        Some("persisted")
    );
}

#[tokio::test]
async fn debug_and_errors_never_contain_plaintext() {
    let store = memory_store().await;
    let accounts = service(&store, 12);
    let request = request("secrets", Some(credential("debug-secret")));
    let debug_request = format!("{request:?}");
    assert!(!debug_request.contains("debug-secret"));
    assert!(debug_request.contains("redacted"));

    let account = accounts.create(request).await.unwrap();
    let context = accounts.authorization_context(&account.id).await.unwrap();
    let debug_context = format!("{context:?}");
    assert!(!debug_context.contains("debug-secret"));
    assert!(debug_context.contains("redacted"));

    let credential = accounts
        .credentials()
        .load(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!format!("{credential:?}").contains("debug-secret"));

    let raw = store
        .credentials()
        .raw_payload(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!raw.contains("debug-secret"));
    assert!(!format!("{:?}", account).contains("debug-secret"));
}

#[tokio::test]
async fn account_without_credential_is_visible_but_not_dispatchable() {
    let store = memory_store().await;
    let accounts = service(&store, 13);
    let account = accounts.create(request("no-secret", None)).await.unwrap();

    let status = accounts
        .credentials()
        .status(&account.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!status.present);
    assert!(accounts
        .authorization_context(&account.id)
        .await
        .unwrap()
        .credential
        .is_none());
    assert!(accounts
        .list_available(ProviderKind::Mock, Region::Cn)
        .await
        .unwrap()
        .is_empty());

    let saved = accounts
        .credentials()
        .save(&account.id, &Credential::from_value(credential("later")))
        .await
        .unwrap();
    assert!(saved.present);
    assert_eq!(
        accounts
            .list_available(ProviderKind::Mock, Region::Cn)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn duplicate_display_names_are_distinct_accounts() {
    let store = memory_store().await;
    let accounts = service(&store, 14);
    let first = accounts
        .create(request("same-name", Some(credential("first"))))
        .await
        .unwrap();
    let second = accounts
        .create(request("same-name", Some(credential("second"))))
        .await
        .unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(first.name, second.name);
    assert_eq!(accounts.list().await.unwrap().len(), 2);
    assert_eq!(
        accounts
            .authorization_context(&first.id)
            .await
            .unwrap()
            .credential
            .unwrap()
            .get("token"),
        Some("first")
    );
}

#[tokio::test]
async fn missing_and_invalid_inputs_fail_without_panicking() {
    let store = memory_store().await;
    let accounts = service(&store, 15);

    assert!(accounts.get("missing").await.unwrap().is_none());
    assert_eq!(
        accounts
            .authorization_context("missing")
            .await
            .unwrap_err()
            .kind,
        ErrorKind::BadRequest
    );
    assert_eq!(
        accounts
            .update("missing", AccountUpdate::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::BadRequest
    );
    assert!(!accounts.delete("missing").await.unwrap());
    assert!(accounts
        .credentials()
        .status("missing")
        .await
        .unwrap()
        .is_none());

    let bad = accounts
        .create(request("bad", Some(serde_json::json!("not-an-object"))))
        .await
        .unwrap_err();
    assert_eq!(bad.kind, ErrorKind::BadRequest);
    assert!(accounts.list().await.unwrap().is_empty());

    let nameless = accounts
        .create(request("   ", Some(credential("x"))))
        .await
        .unwrap_err();
    assert_eq!(nameless.kind, ErrorKind::BadRequest);
}

#[tokio::test]
async fn account_update_changes_metadata_only() {
    let store = memory_store().await;
    let accounts = service(&store, 16);
    let account = accounts
        .create(request("before", Some(credential("token"))))
        .await
        .unwrap();

    let updated = accounts
        .update(
            &account.id,
            AccountUpdate {
                name: Some("after".into()),
                weight: Some(3),
                max_concurrency: Some(2),
                quota_group: Some(Some("shared".into())),
                metadata: Some(serde_json::json!({"team": "a"})),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "after");
    assert_eq!(updated.weight, 3);
    assert_eq!(updated.max_concurrency, 2);
    assert_eq!(updated.quota_group.as_deref(), Some("shared"));
    assert_eq!(updated.metadata, serde_json::json!({"team": "a"}));
    assert_eq!(updated.kind, account.kind);
    assert_eq!(updated.region, account.region);

    let cleared = accounts
        .update(
            &account.id,
            AccountUpdate {
                quota_group: Some(None),
                ..AccountUpdate::default()
            },
        )
        .await
        .unwrap();
    assert!(cleared.quota_group.is_none());

    let bad = accounts
        .update(
            &account.id,
            AccountUpdate {
                weight: Some(0),
                ..AccountUpdate::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.kind, ErrorKind::BadRequest);
}

#[test]
fn legacy_auth_status_strings_are_mapped() {
    assert_eq!(AuthStatus::from_db_str("ok"), AuthStatus::Valid);
    assert_eq!(AuthStatus::from_db_str("valid"), AuthStatus::Valid);
    assert_eq!(AuthStatus::from_db_str("disabled"), AuthStatus::Invalid);
    assert_eq!(
        AuthStatus::from_db_str("refresh_required"),
        AuthStatus::RefreshRequired
    );
    assert_eq!(AuthStatus::from_db_str("nonsense"), AuthStatus::Unknown);
    assert!(AuthStatus::Unknown.can_transition_to(AuthStatus::Valid));
    assert!(!AuthStatus::Invalid.can_transition_to(AuthStatus::Expired));
    assert!(AuthStatus::Invalid.can_transition_to(AuthStatus::Unknown));
}

#[tokio::test]
async fn credential_store_adapter_uses_revision_cas() {
    use super::AccountCredentialStore;
    use crate::providers::CredentialStore;

    let store = memory_store().await;
    let accounts = service(&store, 5);
    let account = accounts
        .create(request("alpha", Some(credential("one"))))
        .await
        .expect("account");
    let adapter = AccountCredentialStore::new(accounts.credentials().clone());

    let (loaded, revision) = adapter
        .load_credential(&account.id)
        .await
        .expect("load")
        .expect("credential");
    assert_eq!(loaded.get("token"), Some("one"));

    let stale = adapter
        .update_credential(
            &account.id,
            revision + 5,
            &Credential::from_value(credential("stale")),
        )
        .await;
    assert!(stale.is_err(), "a stale revision must be rejected");

    let next = adapter
        .update_credential(
            &account.id,
            revision,
            &Credential::from_value(credential("two")),
        )
        .await
        .expect("update");
    assert!(next > revision);
    let (loaded, _) = adapter
        .load_credential(&account.id)
        .await
        .expect("reload")
        .expect("credential");
    assert_eq!(loaded.get("token"), Some("two"));
}
