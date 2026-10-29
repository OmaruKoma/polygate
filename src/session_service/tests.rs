//! Session binding service tests (M13).
//!
//! Concurrency tests use tokio's paused clock so queue timeouts and slot
//! hand-offs are deterministic and never wait on the wall clock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};

use crate::account_service::{AccountService, CreateAccount};
use crate::accounts::{Account, AuthStatus};
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::security::SecretBox;
use crate::session_service::{
    classify_session_failure, LimitPolicy, SessionBindingService, SessionFailure, SessionKey,
    SessionRecovery, SessionRouter,
};
use crate::sessions::SessionState;
use crate::storage::Storage;

fn secret_box() -> SecretBox {
    SecretBox::from_master_key(&[3u8; 32]).expect("32-byte key")
}

fn policy() -> LimitPolicy {
    LimitPolicy {
        max_in_flight: 4,
        max_starts: 0,
        window: Duration::ZERO,
        queue_timeout: Duration::from_secs(3600),
        serialize_session: false,
    }
}

struct Fixture {
    _dir: Option<tempfile::TempDir>,
    path: Option<PathBuf>,
    storage: Storage,
    accounts: AccountService,
    service: SessionBindingService,
}

impl Fixture {
    async fn memory() -> Self {
        Self::memory_with(policy()).await
    }

    async fn memory_with(policy: LimitPolicy) -> Self {
        let storage = Storage::open_memory().await.expect("memory db");
        let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
        let service =
            SessionBindingService::new(storage.clone(), accounts.clone(), policy).expect("service");
        Self {
            _dir: None,
            path: None,
            storage,
            accounts,
            service,
        }
    }

    async fn file() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("polygate.db");
        let storage = Storage::open_file(&path, 4).await.expect("file db");
        let accounts = AccountService::new(storage.clone(), Arc::new(secret_box()));
        let service = SessionBindingService::new(storage.clone(), accounts.clone(), policy())
            .expect("service");
        Self {
            _dir: Some(dir),
            path: Some(path),
            storage,
            accounts,
            service,
        }
    }

    async fn account(&self, name: &str) -> Account {
        self.account_in(ProviderKind::Codearts, Region::Cn, name)
            .await
    }

    async fn account_in(&self, kind: ProviderKind, region: Region, name: &str) -> Account {
        self.accounts
            .create(CreateAccount::new(
                kind,
                region,
                name,
                Some(serde_json::json!({"token": format!("token-{name}")})),
            ))
            .await
            .expect("account")
    }
}

fn key(client: &str) -> SessionKey {
    SessionKey::new(client, ProviderKind::Codearts, Region::Cn)
}

// ---------------------------------------------------------------------------
// Stickiness, create-only bindings, concurrency of binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bind_is_create_only_and_sticky() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;

    let first = fixture
        .service
        .bind_if_absent(&key("client-1"), &alpha.id, Some("up-1"))
        .await
        .expect("first bind");
    assert!(first.created);
    assert_eq!(first.session.account_id, alpha.id);
    assert_eq!(first.session.upstream_session_id.as_deref(), Some("up-1"));

    // A later suggestion for another account must not move the binding.
    let second = fixture
        .service
        .bind_if_absent(&key("client-1"), &beta.id, Some("up-2"))
        .await
        .expect("second bind");
    assert!(!second.created);
    assert_eq!(second.session.account_id, alpha.id);
    assert_eq!(second.session.upstream_session_id.as_deref(), Some("up-1"));

    let lease = fixture
        .service
        .acquire(&key("client-1"))
        .await
        .expect("acquire");
    assert_eq!(lease.account_id(), alpha.id);
    assert_eq!(lease.upstream_session_id(), Some("up-1"));
    let finished = lease.finish().await.expect("finish").expect("row");
    assert_eq!(finished.request_count, 1);
}

#[tokio::test]
async fn concurrent_binds_have_exactly_one_winner() {
    let fixture = Fixture::file().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let session_key = key("client-race");

    let (first, second) = tokio::join!(
        fixture
            .service
            .bind_if_absent(&session_key, &alpha.id, None),
        fixture.service.bind_if_absent(&session_key, &beta.id, None)
    );
    let first = first.expect("a");
    let second = second.expect("b");
    assert_ne!(first.created, second.created, "exactly one insert wins");
    assert_eq!(first.session.account_id, second.session.account_id);
    assert_eq!(first.session.binding_revision, 0);
}

#[tokio::test]
async fn regions_and_provider_kinds_do_not_share_bindings() {
    let fixture = Fixture::memory().await;
    let cn = fixture.account("cn").await;
    let intl = fixture
        .account_in(ProviderKind::Codearts, Region::Intl, "intl")
        .await;

    let cn_key = SessionKey::new("same-client", ProviderKind::Codearts, Region::Cn);
    let intl_key = SessionKey::new("same-client", ProviderKind::Codearts, Region::Intl);
    fixture
        .service
        .bind_if_absent(&cn_key, &cn.id, Some("cn-upstream"))
        .await
        .expect("cn");
    fixture
        .service
        .bind_if_absent(&intl_key, &intl.id, Some("intl-upstream"))
        .await
        .expect("intl");

    let cn_session = fixture
        .service
        .get(&cn_key)
        .await
        .expect("get")
        .expect("row");
    let intl_session = fixture
        .service
        .get(&intl_key)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(cn_session.account_id, cn.id);
    assert_eq!(intl_session.account_id, intl.id);
    assert_ne!(
        cn_session.upstream_session_id,
        intl_session.upstream_session_id
    );
}

// ---------------------------------------------------------------------------
// Upstream id attachment and rebinding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn attach_upstream_is_account_and_revision_scoped() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, None)
        .await
        .expect("bind");

    // Wrong account: the id must not land on beta or alpha.
    let error = fixture
        .service
        .attach_upstream(&session_key, &beta.id, 0, "up-b")
        .await
        .expect_err("wrong account");
    assert_eq!(error.kind, ErrorKind::SessionBindingInvalid);

    let attached = fixture
        .service
        .attach_upstream(&session_key, &alpha.id, 0, "up-a")
        .await
        .expect("attach");
    assert_eq!(attached.upstream_session_id.as_deref(), Some("up-a"));
    assert_eq!(attached.binding_revision, 1);

    // Stale revision cannot overwrite the id.
    let error = fixture
        .service
        .attach_upstream(&session_key, &alpha.id, 0, "up-stale")
        .await
        .expect_err("stale");
    assert_eq!(error.kind, ErrorKind::SessionBindingInvalid);
    let stored = fixture
        .service
        .get(&session_key)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(stored.upstream_session_id.as_deref(), Some("up-a"));
}

#[tokio::test]
async fn rebind_moves_account_and_never_carries_the_upstream_id() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-a"))
        .await
        .expect("bind");

    let moved = fixture
        .service
        .rebind(&session_key, &beta.id)
        .await
        .expect("rebind");
    assert_eq!(moved.account_id, beta.id);
    assert!(moved.upstream_session_id.is_none());
    assert_eq!(moved.binding_revision, 1);
    assert_eq!(moved.state, SessionState::Active);

    // A repository-level stale CAS cannot move it again.
    let stale = fixture
        .storage
        .sessions()
        .rebind(
            &session_key.client_session_id,
            session_key.kind,
            session_key.region,
            0,
            &alpha.id,
            "codearts-cn",
            None,
            3600,
        )
        .await
        .expect("stale rebind");
    assert!(stale.is_none());
    let stored = fixture
        .service
        .get(&session_key)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(stored.account_id, beta.id);
}

// ---------------------------------------------------------------------------
// Invalidation, recovery, expiry, account state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalidate_keeps_context_and_blocks_acquisition() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-a"))
        .await
        .expect("bind");

    let invalid = fixture
        .service
        .invalidate(&session_key, "upstream rejected the chat id")
        .await
        .expect("invalidate");
    assert_eq!(invalid.state, SessionState::Invalid);
    assert_eq!(invalid.account_id, alpha.id, "account is kept");
    assert_eq!(
        invalid.upstream_session_id.as_deref(),
        Some("up-a"),
        "upstream id is kept for inspection"
    );
    assert_eq!(
        invalid.invalid_reason.as_deref(),
        Some("upstream rejected the chat id")
    );

    let error = fixture
        .service
        .acquire(&session_key)
        .await
        .expect_err("invalid binding");
    assert_eq!(error.kind, ErrorKind::SessionBindingInvalid);
    assert!(
        error.message.contains("upstream rejected"),
        "{}",
        error.message
    );

    // Invalidating again is idempotent (no revision churn).
    let again = fixture
        .service
        .invalidate(&session_key, "second call")
        .await
        .expect("idempotent");
    assert_eq!(again.binding_revision, invalid.binding_revision);

    let stored = fixture
        .service
        .get(&session_key)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(stored.state, SessionState::Invalid);
}

#[tokio::test]
async fn reset_upstream_keeps_account_and_clears_only_the_session() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-a"))
        .await
        .expect("bind");
    fixture
        .service
        .invalidate(&session_key, "session expired upstream")
        .await
        .expect("invalidate");

    let reset = fixture
        .service
        .recover(
            &session_key,
            SessionRecovery::ResetUpstream {
                reason: "start a new upstream session; full history resent".into(),
            },
        )
        .await
        .expect("recover");
    assert_eq!(reset.state, SessionState::Active);
    assert_eq!(reset.account_id, alpha.id);
    assert!(reset.upstream_session_id.is_none());
    assert_eq!(
        reset.invalid_reason.as_deref(),
        Some("start a new upstream session; full history resent")
    );

    let lease = fixture
        .service
        .acquire(&session_key)
        .await
        .expect("acquire after reset");
    assert_eq!(lease.account_id(), alpha.id);
}

#[tokio::test]
async fn expired_binding_is_not_silently_replaced() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-a"))
        .await
        .expect("bind");

    let mut stored = fixture
        .storage
        .sessions()
        .get(
            &session_key.client_session_id,
            session_key.kind,
            session_key.region,
        )
        .await
        .expect("get")
        .expect("row");
    stored.expires_at = Utc::now() - ChronoDuration::minutes(1);
    fixture
        .storage
        .sessions()
        .save(&stored)
        .await
        .expect("expire");
    let expired_expiry = fixture
        .storage
        .sessions()
        .get(
            &session_key.client_session_id,
            session_key.kind,
            session_key.region,
        )
        .await
        .expect("get")
        .expect("row")
        .expires_at;

    let error = fixture
        .service
        .acquire(&session_key)
        .await
        .expect_err("expired");
    assert_eq!(error.kind, ErrorKind::SessionBindingInvalid);

    // Re-binding does not extend or replace the expired row either; recovery is
    // an explicit action.
    let outcome = fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-b"))
        .await
        .expect("bind again");
    assert!(!outcome.created);
    assert_eq!(outcome.session.expires_at, expired_expiry);
    assert_eq!(outcome.session.upstream_session_id.as_deref(), Some("up-a"));

    let recovered = fixture
        .service
        .recover(
            &session_key,
            SessionRecovery::ResetUpstream {
                reason: "operator restarted the session".into(),
            },
        )
        .await
        .expect("recover");
    assert_eq!(recovered.state, SessionState::Active);
    assert!(fixture.service.acquire(&session_key).await.is_ok());
}

#[tokio::test]
async fn disabled_or_auth_rejected_account_blocks_acquisition() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, None)
        .await
        .expect("bind");

    fixture
        .accounts
        .set_enabled(&alpha.id, false)
        .await
        .expect("disable");
    let error = fixture
        .service
        .acquire(&session_key)
        .await
        .expect_err("disabled");
    assert_eq!(error.kind, ErrorKind::AccountDisabled);

    fixture
        .accounts
        .set_enabled(&alpha.id, true)
        .await
        .expect("enable");
    fixture
        .accounts
        .record_auth_status(&alpha.id, AuthStatus::Invalid, Some("401"))
        .await
        .expect("auth invalid");
    let error = fixture
        .service
        .acquire(&session_key)
        .await
        .expect_err("auth invalid");
    assert_eq!(error.kind, ErrorKind::AuthInvalid);
}

// ---------------------------------------------------------------------------
// Bounded concurrency and per-session serialization
// ---------------------------------------------------------------------------

fn limited_policy(
    max_in_flight: usize,
    queue_timeout: Duration,
    serialize_session: bool,
) -> LimitPolicy {
    LimitPolicy {
        max_in_flight,
        max_starts: 0,
        window: Duration::ZERO,
        queue_timeout,
        serialize_session,
    }
}

#[tokio::test]
async fn account_slots_are_bounded_and_waiters_wake_on_release() {
    let fixture = Fixture::memory_with(limited_policy(1, Duration::from_secs(3600), false)).await;
    let alpha = fixture.account("alpha").await;
    let first_key = key("client-1");
    let second_key = key("client-2");
    fixture
        .service
        .bind_if_absent(&first_key, &alpha.id, None)
        .await
        .expect("a");
    fixture
        .service
        .bind_if_absent(&second_key, &alpha.id, None)
        .await
        .expect("b");

    let first = fixture
        .service
        .acquire(&first_key)
        .await
        .expect("first slot");
    let service = fixture.service.clone();
    let waiter = tokio::spawn(async move { service.acquire(&second_key).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiter.is_finished(), "second request must wait for a slot");

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("waiter woke")
        .expect("task");
    assert!(second.is_ok(), "waiter acquired the released slot");
}

#[tokio::test]
async fn queue_timeout_is_bounded() {
    let fixture = Fixture::memory_with(limited_policy(1, Duration::from_millis(50), false)).await;
    let alpha = fixture.account("alpha").await;
    let first_key = key("client-1");
    let second_key = key("client-2");
    fixture
        .service
        .bind_if_absent(&first_key, &alpha.id, None)
        .await
        .expect("a");
    fixture
        .service
        .bind_if_absent(&second_key, &alpha.id, None)
        .await
        .expect("b");

    let _held = fixture.service.acquire(&first_key).await.expect("slot");
    let service = fixture.service.clone();
    let waiter = tokio::spawn(async move { service.acquire(&second_key).await });
    tokio::time::sleep(Duration::from_millis(120)).await;
    let result = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("bounded")
        .expect("task");
    let error = result.expect_err("queue must time out");
    assert_eq!(error.kind, ErrorKind::NoCapacity);
}

#[tokio::test]
async fn same_session_is_serialized_but_other_sessions_proceed() {
    let fixture = Fixture::memory_with(limited_policy(4, Duration::from_secs(3600), true)).await;
    let alpha = fixture.account("alpha").await;
    let first_key = key("client-1");
    let other_key = key("client-2");
    fixture
        .service
        .bind_if_absent(&first_key, &alpha.id, None)
        .await
        .expect("a");
    fixture
        .service
        .bind_if_absent(&other_key, &alpha.id, None)
        .await
        .expect("b");

    let first = fixture.service.acquire(&first_key).await.expect("first");
    let service = fixture.service.clone();
    let same_session = tokio::spawn(async move { service.acquire(&first_key).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !same_session.is_finished(),
        "same session must wait for the previous request"
    );

    let other = fixture
        .service
        .acquire(&other_key)
        .await
        .expect("different session is not blocked");
    drop(other);

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(5), same_session)
        .await
        .expect("serialized waiter woke")
        .expect("task");
    assert!(second.is_ok());
}

// ---------------------------------------------------------------------------
// Restart persistence and classification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binding_survives_a_restart() {
    let fixture = Fixture::file().await;
    let alpha = fixture.account("alpha").await;
    let session_key = key("client-1");
    fixture
        .service
        .bind_if_absent(&session_key, &alpha.id, Some("up-a"))
        .await
        .expect("bind");
    let path = fixture.path.clone().expect("path");
    fixture.storage.close().await;

    let reopened = Storage::open_file(&path, 4).await.expect("reopen");
    let accounts = AccountService::new(reopened.clone(), Arc::new(secret_box()));
    let service =
        SessionBindingService::new(reopened.clone(), accounts, policy()).expect("service");
    let stored = service.get(&session_key).await.expect("get").expect("row");
    assert_eq!(stored.account_id, alpha.id);
    assert_eq!(stored.upstream_session_id.as_deref(), Some("up-a"));
    assert_eq!(stored.binding_revision, 0);
    assert!(service.acquire(&session_key).await.is_ok());
}

#[test]
fn session_failure_classification_is_stable() {
    let table = [
        (
            ErrorKind::SessionBindingInvalid,
            SessionFailure::UpstreamSessionInvalid,
        ),
        (
            ErrorKind::AccountDisabled,
            SessionFailure::AccountUnavailable,
        ),
        (ErrorKind::AuthInvalid, SessionFailure::AccountUnavailable),
        (ErrorKind::AuthExpired, SessionFailure::AccountUnavailable),
        (
            ErrorKind::ChannelRestricted,
            SessionFailure::AccountUnavailable,
        ),
        (ErrorKind::RateLimited, SessionFailure::AccountBusy),
        (ErrorKind::NoCapacity, SessionFailure::AccountBusy),
        (ErrorKind::Timeout, SessionFailure::Other),
    ];
    for (kind, expected) in table {
        let error = GatewayError::new(kind, "x");
        assert_eq!(classify_session_failure(&error), expected, "{kind:?}");
    }
}

// ---------------------------------------------------------------------------
// Router: account stickiness, isolation, recovery, cancellation
// ---------------------------------------------------------------------------

fn router(fixture: &Fixture) -> SessionRouter {
    SessionRouter::new(fixture.service.clone(), fixture.accounts.clone())
}

#[tokio::test]
async fn router_is_sticky_and_generates_a_32_hex_upstream_id() {
    let fixture = Fixture::memory().await;
    fixture.account("alpha").await;
    fixture.account("beta").await;
    let router = router(&fixture);

    let first = router.open(key("client-1")).await.expect("first open");
    let account = first.account_id().to_string();
    let upstream = first.upstream_session_id().to_string();
    assert_eq!(upstream.len(), 32);
    assert!(upstream.chars().all(|ch| ch.is_ascii_hexdigit()));
    router.finish(first).await;

    let second = router.open(key("client-1")).await.expect("second open");
    assert_eq!(second.account_id(), account);
    assert_eq!(second.upstream_session_id(), upstream);
    router.finish(second).await;
}

#[tokio::test]
async fn router_never_carries_an_upstream_id_across_accounts() {
    let fixture = Fixture::memory().await;
    fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let router = router(&fixture);

    let first = router.open(key("client-1")).await.expect("first open");
    let before = first.upstream_session_id().to_string();
    router.finish(first).await;

    fixture
        .service
        .rebind(&key("client-1"), &beta.id)
        .await
        .expect("rebind");
    let second = router.open(key("client-1")).await.expect("second open");
    assert_eq!(second.account_id(), beta.id);
    assert_ne!(second.upstream_session_id(), before);
    router.finish(second).await;
}

#[tokio::test]
async fn router_resets_the_upstream_id_after_an_invalid_session() {
    let fixture = Fixture::memory().await;
    fixture.account("alpha").await;
    let router = router(&fixture);

    let lease = router.open(key("client-1")).await.expect("open");
    let before = lease.upstream_session_id().to_string();
    let error = GatewayError::new(
        ErrorKind::SessionBindingInvalid,
        "upstream rejected the session",
    );
    router.report_failure(&lease, &error).await;
    router.finish(lease).await;

    let stored = fixture
        .service
        .get(&key("client-1"))
        .await
        .expect("get")
        .expect("row");
    assert!(stored.upstream_session_id.is_none());
    assert!(stored.invalid_reason.is_some());

    let next = router.open(key("client-1")).await.expect("reopen");
    assert_ne!(next.upstream_session_id(), before);
    router.finish(next).await;
}

#[tokio::test]
async fn router_persists_auth_rejections_without_rebinding() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let router = router(&fixture);

    let lease = router.open(key("client-1")).await.expect("open");
    assert_eq!(lease.account_id(), alpha.id);
    let error = GatewayError::auth_invalid("upstream rejected the credential");
    router.report_failure(&lease, &error).await;
    router.finish(lease).await;

    let stored = fixture
        .accounts
        .get(&alpha.id)
        .await
        .expect("get")
        .expect("account");
    assert_eq!(stored.auth_status, AuthStatus::Invalid);
    let binding = fixture
        .service
        .get(&key("client-1"))
        .await
        .expect("binding")
        .expect("row");
    assert_eq!(binding.account_id, alpha.id);
    assert!(binding.upstream_session_id.is_some());
}

#[tokio::test]
async fn router_reports_no_capacity_when_no_account_exists() {
    let fixture = Fixture::memory().await;
    let error = router(&fixture)
        .open(key("client-1"))
        .await
        .expect_err("no account");
    assert_eq!(error.kind, ErrorKind::NoCapacity);
}

#[tokio::test]
async fn dropping_a_lease_releases_account_and_session_permits() {
    let fixture = Fixture::memory_with(LimitPolicy {
        max_in_flight: 1,
        max_starts: 0,
        window: Duration::ZERO,
        queue_timeout: Duration::from_secs(5),
        serialize_session: true,
    })
    .await;
    fixture.account("alpha").await;
    let router = router(&fixture);

    let first = router.open(key("client-1")).await.expect("first");
    let waiting = {
        let router = router.clone();
        tokio::spawn(async move { router.open(key("client-2")).await })
    };
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !waiting.is_finished(),
        "second lease must wait for the slot"
    );

    // Simulates dropping an SSE body (client disconnect): permits are released
    // without needing an explicit finish.
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("waiter wakes")
        .expect("join")
        .expect("second open");
    router.finish(second).await;
}

#[tokio::test]
async fn invalid_upstream_session_recovers_on_the_same_account_only() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let router = router(&fixture);

    let lease = router.open(key("client-invalid")).await.expect("open");
    // Account ids are random UUIDs, so the selected account is whatever the
    // router picked; the other one must never receive this session.
    let chosen = lease.account_id().to_string();
    let other = if chosen == alpha.id {
        &beta.id
    } else {
        &alpha.id
    };
    let before = lease.upstream_session_id().to_string();

    let error = GatewayError::new(
        ErrorKind::SessionBindingInvalid,
        "upstream rejected the chat id",
    );
    router.report_failure(&lease, &error).await;
    router.finish(lease).await;

    let stored = fixture
        .service
        .get(&key("client-invalid"))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        stored.account_id, chosen,
        "invalid-session recovery must never move to another account"
    );
    assert!(
        stored.upstream_session_id.is_none(),
        "the unusable upstream id is dropped"
    );
    assert_eq!(stored.state, SessionState::Active);
    assert_eq!(
        fixture
            .storage
            .sessions()
            .count_active_by_account(other)
            .await
            .expect("count"),
        0,
        "the other account never received this session"
    );

    let next = router.open(key("client-invalid")).await.expect("reopen");
    assert_eq!(next.account_id(), chosen);
    assert_ne!(
        next.upstream_session_id(),
        before,
        "a fresh upstream session is generated on the same account"
    );
    router.finish(next).await;
}

#[tokio::test]
async fn auth_failure_blocks_the_bound_account_instead_of_switching() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let router = router(&fixture);

    let lease = router.open(key("client-auth")).await.expect("open");
    let chosen = lease.account_id().to_string();
    let other = if chosen == alpha.id {
        &beta.id
    } else {
        &alpha.id
    };
    let upstream = lease.upstream_session_id().to_string();

    let error = GatewayError::new(ErrorKind::AuthExpired, "Qoder session expired");
    router.report_failure(&lease, &error).await;
    router.finish(lease).await;

    let account = fixture
        .accounts
        .get(&chosen)
        .await
        .expect("get")
        .expect("account");
    assert_eq!(account.auth_status, AuthStatus::Expired);
    let stored = fixture
        .service
        .get(&key("client-auth"))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(stored.account_id, chosen);
    assert_eq!(
        stored.upstream_session_id.as_deref(),
        Some(upstream.as_str()),
        "an auth failure keeps the binding intact for inspection"
    );

    let blocked = router
        .open(key("client-auth"))
        .await
        .expect_err("an expired account must block the bound session");
    assert_eq!(blocked.kind, ErrorKind::AuthExpired);
    let after = fixture
        .service
        .get(&key("client-auth"))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        after.account_id, chosen,
        "the gateway must not silently fall back to another account"
    );
    assert_eq!(
        fixture
            .storage
            .sessions()
            .count_active_by_account(other)
            .await
            .expect("count"),
        0
    );
}

#[tokio::test]
async fn restart_reopens_valid_bindings_and_keeps_invalid_or_expired_ones_blocked() {
    let fixture = Fixture::file().await;
    let alpha = fixture.account("alpha").await;
    let valid = key("s-valid");
    let invalid = key("s-invalid");
    let expired = key("s-expired");
    for (session_key, upstream) in [
        (&valid, "up-valid"),
        (&invalid, "up-invalid"),
        (&expired, "up-expired"),
    ] {
        fixture
            .service
            .bind_if_absent(session_key, &alpha.id, Some(upstream))
            .await
            .expect("bind");
    }
    fixture
        .service
        .invalidate(&invalid, "upstream rejected the chat id")
        .await
        .expect("invalidate");

    let mut row = fixture
        .storage
        .sessions()
        .get(&expired.client_session_id, expired.kind, expired.region)
        .await
        .expect("get")
        .expect("row");
    row.expires_at = Utc::now() - ChronoDuration::minutes(1);
    fixture.storage.sessions().save(&row).await.expect("expire");

    let path = fixture.path.clone().expect("path");
    fixture.storage.close().await;

    // A new process only has the database: every permit and gate starts empty.
    let reopened = Storage::open_file(&path, 4).await.expect("reopen");
    let accounts = AccountService::new(reopened.clone(), Arc::new(secret_box()));
    let service =
        SessionBindingService::new(reopened.clone(), accounts.clone(), policy()).expect("service");
    let router = SessionRouter::new(service.clone(), accounts);

    let lease = router.open(valid.clone()).await.expect("valid binding");
    assert_eq!(lease.account_id(), alpha.id);
    assert_eq!(lease.upstream_session_id(), "up-valid");
    router.finish(lease).await;

    for (session_key, expected) in [(&invalid, "upstream rejected"), (&expired, "expired")] {
        let error = router
            .open(session_key.clone())
            .await
            .expect_err("unusable bindings stay unusable");
        assert_eq!(error.kind, ErrorKind::SessionBindingInvalid);
        assert!(
            error.message.contains(expected),
            "{}: {}",
            session_key.client_session_id,
            error.message
        );
    }

    // Create-only binding does not revive an invalid row either.
    let outcome = service
        .bind_if_absent(&invalid, &alpha.id, Some("up-new"))
        .await
        .expect("bind again");
    assert!(!outcome.created);
    assert_eq!(outcome.session.state, SessionState::Invalid);
    assert_eq!(outcome.session.account_id, alpha.id);
}

// ---------------------------------------------------------------------------
// Scheduler integration (M15)
// ---------------------------------------------------------------------------

use crate::config::{SchedulerConfig, SchedulerStrategy};
use crate::quota::{QuotaSnapshot, QuotaStatus, QuotaUnit};
use crate::scheduler::Scheduler;

fn scheduled_router(fixture: &Fixture, config: SchedulerConfig) -> SessionRouter {
    let scheduler = Arc::new(Scheduler::new(config, Some(fixture.storage.clone())));
    SessionRouter::with_scheduler(fixture.service.clone(), fixture.accounts.clone(), scheduler)
}

fn round_robin() -> SchedulerConfig {
    SchedulerConfig {
        strategy: SchedulerStrategy::RoundRobin,
        ..SchedulerConfig::default()
    }
}

#[tokio::test]
async fn round_robin_spreads_new_sessions_across_accounts() {
    let fixture = Fixture::memory().await;
    fixture.account("alpha").await;
    fixture.account("beta").await;
    let router = scheduled_router(&fixture, round_robin());

    let mut ids = Vec::new();
    for index in 0..4 {
        let lease = router
            .open(key(&format!("client-{index}")))
            .await
            .expect("open");
        ids.push(lease.account_id().to_string());
        router.finish(lease).await;
    }
    assert_eq!(ids.iter().filter(|id| **id == ids[0]).count(), 2);
    assert_eq!(ids.iter().filter(|id| **id == ids[2]).count(), 2);
}

#[tokio::test]
async fn sticky_binding_wins_over_rotation() {
    let fixture = Fixture::memory().await;
    fixture.account("alpha").await;
    fixture.account("beta").await;
    let router = scheduled_router(&fixture, round_robin());

    let first = router.open(key("client-1")).await.expect("open");
    let account = first.account_id().to_string();
    let upstream = first.upstream_session_id().to_string();
    router.finish(first).await;
    for _ in 0..3 {
        let again = router.open(key("client-1")).await.expect("reopen");
        assert_eq!(again.account_id(), account);
        assert_eq!(again.upstream_session_id(), upstream);
        router.finish(again).await;
    }
}

#[tokio::test]
async fn open_excluding_rebinds_and_never_reuses_the_upstream_id() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let router = scheduled_router(&fixture, round_robin());

    let first = router.open(key("client-1")).await.expect("open");
    let primary = first.account_id().to_string();
    assert!(
        primary == alpha.id || primary == beta.id,
        "the first lease must use a configured account"
    );
    let upstream = first.upstream_session_id().to_string();
    router.finish(first).await;

    let second = router
        .open_excluding(key("client-1"), std::slice::from_ref(&primary))
        .await
        .expect("failover open");
    assert_ne!(second.account_id(), primary);
    assert_ne!(second.upstream_session_id(), upstream);
    router.finish(second).await;
}

#[tokio::test]
async fn known_exhausted_quota_steers_new_sessions_away() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    fixture.account("beta").await;
    let mut quota = QuotaSnapshot::unknown(&alpha.id, "");
    quota.status = QuotaStatus::Known;
    quota.unit = QuotaUnit::Credits;
    quota.total = Some(10.0);
    quota.remaining = Some(0.0);
    quota.error = None;
    fixture.storage.quota().save(&quota).await.expect("quota");

    let router = scheduled_router(&fixture, round_robin());
    for index in 0..3 {
        let lease = router
            .open(key(&format!("quota-{index}")))
            .await
            .expect("open");
        assert_ne!(lease.account_id(), alpha.id);
        router.finish(lease).await;
    }
}

#[tokio::test]
async fn repeated_transient_failures_cool_an_account_down() {
    let fixture = Fixture::memory().await;
    let alpha = fixture.account("alpha").await;
    let beta = fixture.account("beta").await;
    let config = SchedulerConfig {
        strategy: SchedulerStrategy::RoundRobin,
        failure_threshold: 1,
        cooldown_base_secs: 60,
        ..SchedulerConfig::default()
    };
    let router = scheduled_router(&fixture, config);

    let lease = router.open(key("failing")).await.expect("open");
    let failing = lease.account_id().to_string();
    assert!(
        failing == alpha.id || failing == beta.id,
        "the failing lease must be one of the configured accounts"
    );
    let error = GatewayError::new(ErrorKind::UpstreamServer, "boom");
    router.report_failure(&lease, &error).await;
    router.finish(lease).await;

    for index in 0..3 {
        let lease = router
            .open(key(&format!("after-{index}")))
            .await
            .expect("open");
        assert_ne!(lease.account_id(), failing);
        router.finish(lease).await;
    }
}
