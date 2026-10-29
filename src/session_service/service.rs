//! Persistent session → account → upstream-session binding.
//!
//! This is the policy layer over [`SessionRepo`]. It deliberately does not pick
//! accounts: the future scheduler chooses a candidate and calls
//! [`SessionBindingService::bind_if_absent`] once per downstream session. After
//! that, every request for the same `(client session, kind, region)` sees the
//! same account until an explicit rebind.
//!
//! Invariants:
//!
//! * `(client_session_id, kind, region)` is the only lookup key; CN and Intl
//!   bindings of the same client id are separate rows and never share an
//!   upstream id.
//! * every mutation is compare-and-swap on `binding_revision`; a stale writer
//!   receives `session_binding_invalid` instead of silently moving a live
//!   session to another account or overwriting its upstream id;
//! * invalidation keeps the account and upstream id on disk for inspection; it
//!   never deletes the row and never silently re-creates it;
//! * recovery is explicit: keep the account and start a new upstream session
//!   (full history must be resent) or move to a new account (upstream id is
//!   cleared).

use std::time::Duration;

use chrono::Utc;

use crate::account_service::AccountService;
use crate::accounts::AuthStatus;
use crate::config::{ProviderKind, Region};
use crate::error::{redact_secrets, ErrorKind, GatewayError};
use crate::sessions::{Session, SessionState};
use crate::storage::Storage;

use super::limits::{AccountLease, AccountLimiter, LimitPolicy, SessionGate, SessionGateGuard};

const MAX_CLIENT_SESSION_ID: usize = 200;
const MAX_REASON_CHARS: usize = 512;

/// Lookup key for one downstream session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub client_session_id: String,
    pub kind: ProviderKind,
    pub region: Region,
}

impl SessionKey {
    pub fn new(client_session_id: impl Into<String>, kind: ProviderKind, region: Region) -> Self {
        Self {
            client_session_id: client_session_id.into().trim().to_string(),
            kind,
            region,
        }
    }

    fn validate(&self) -> Result<(), GatewayError> {
        let id = self.client_session_id.trim();
        if id.is_empty() {
            return Err(GatewayError::bad_request(
                "client session id must not be empty",
            ));
        }
        if id.chars().count() > MAX_CLIENT_SESSION_ID {
            return Err(GatewayError::bad_request(
                "client session id is too long (maximum 200 characters)",
            ));
        }
        Ok(())
    }

    fn gate_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.kind.as_str(),
            self.region.as_str(),
            self.client_session_id
        )
    }
}

#[derive(Debug, Clone)]
pub struct BindOutcome {
    pub session: Session,
    /// `false` means the binding already existed and was returned unchanged.
    pub created: bool,
}

/// Explicit recovery after an upstream session became unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRecovery {
    /// Keep the same account but start a new upstream session. The provider
    /// adapter must resend the full message history; the old id is gone.
    ResetUpstream { reason: String },
    /// Move the binding to another account. The upstream session id is cleared.
    Rebind { account_id: String },
}

/// Coarse classification used by caller-side retry/recovery decisions.
///
/// There is no verified CodeArts string for "this chat session expired" in the
/// reference sources; an adapter that observes one must map it to
/// `ErrorKind::SessionBindingInvalid`, which this function turns into
/// [`SessionFailure::UpstreamSessionInvalid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFailure {
    UpstreamSessionInvalid,
    AccountUnavailable,
    AccountBusy,
    Other,
}

pub fn classify_session_failure(error: &GatewayError) -> SessionFailure {
    match error.kind {
        ErrorKind::SessionBindingInvalid => SessionFailure::UpstreamSessionInvalid,
        ErrorKind::AccountDisabled
        | ErrorKind::ChannelRestricted
        | ErrorKind::AuthInvalid
        | ErrorKind::AuthExpired => SessionFailure::AccountUnavailable,
        ErrorKind::RateLimited | ErrorKind::NoCapacity => SessionFailure::AccountBusy,
        _ => SessionFailure::Other,
    }
}

/// A binding plus the concurrency permits held for one request.
///
/// Dropping the lease releases both the per-session lock and the account slot.
/// Call [`SessionLease::finish`] after the upstream call to extend the binding
/// TTL and count the request; dropping without finishing is safe but does not
/// touch usage.
pub struct SessionLease {
    storage: Storage,
    key: SessionKey,
    session: Session,
    ttl: Duration,
    _session_guard: Option<SessionGateGuard>,
    _account_lease: AccountLease,
}

impl std::fmt::Debug for SessionLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLease")
            .field("account_id", &self.session.account_id)
            .field(
                "upstream_session_id",
                &self.session.upstream_session_id.as_ref().map(|_| "<set>"),
            )
            .field("binding_revision", &self.session.binding_revision)
            .finish_non_exhaustive()
    }
}

impl SessionLease {
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn account_id(&self) -> &str {
        &self.session.account_id
    }

    pub fn upstream_session_id(&self) -> Option<&str> {
        self.session.upstream_session_id.as_deref()
    }

    /// Extend the binding TTL and return the refreshed row. The permits stay
    /// held until this future completes, so the next request of the same
    /// session only starts after the caller recorded completion.
    pub async fn finish(self) -> Result<Option<Session>, GatewayError> {
        let _ = self
            .storage
            .sessions()
            .touch(&self.session.id, self.ttl.as_secs())
            .await?;
        self.storage
            .sessions()
            .get(&self.key.client_session_id, self.key.kind, self.key.region)
            .await
    }
}

#[derive(Clone)]
pub struct SessionBindingService {
    storage: Storage,
    accounts: AccountService,
    policy: LimitPolicy,
    ttl: Duration,
    limiter: AccountLimiter,
    gate: SessionGate,
}

impl SessionBindingService {
    pub fn new(
        storage: Storage,
        accounts: AccountService,
        policy: LimitPolicy,
    ) -> Result<Self, GatewayError> {
        let policy = policy.validate()?;
        Ok(Self {
            storage,
            accounts,
            policy,
            ttl: Duration::from_secs(24 * 3600),
            limiter: AccountLimiter::new(policy),
            gate: SessionGate::default(),
        })
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn policy(&self) -> LimitPolicy {
        self.policy
    }

    /// Create a binding if none exists for the key. An existing binding is
    /// returned unchanged even if it is bound to another account: stickiness
    /// wins over the caller's suggestion, and moving it requires an explicit
    /// [`Self::rebind`].
    pub async fn bind_if_absent(
        &self,
        key: &SessionKey,
        account_id: &str,
        upstream_session_id: Option<&str>,
    ) -> Result<BindOutcome, GatewayError> {
        key.validate()?;
        let account = self.account_for(key, account_id).await?;
        let now = Utc::now();
        let candidate = Session {
            id: String::new(),
            client_session_id: key.client_session_id.trim().to_string(),
            provider: account.provider,
            kind: key.kind,
            region: key.region.as_str().to_string(),
            account_id: account.id,
            upstream_session_id: upstream_session_id.map(str::to_string),
            state: SessionState::Active,
            binding_revision: 0,
            invalid_reason: None,
            created_at: now,
            last_used_at: now,
            expires_at: now + chrono::Duration::seconds(self.ttl.as_secs() as i64),
            request_count: 0,
            metadata: serde_json::json!({}),
        };
        if self.storage.sessions().insert_if_absent(&candidate).await? {
            let stored = self
                .storage
                .sessions()
                .get(&key.client_session_id, key.kind, key.region)
                .await?
                .ok_or_else(|| GatewayError::internal("session disappeared after insert"))?;
            return Ok(BindOutcome {
                session: stored,
                created: true,
            });
        }
        let existing = self
            .storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await?
            .ok_or_else(|| GatewayError::internal("session disappeared after insert conflict"))?;
        Ok(BindOutcome {
            session: existing,
            created: false,
        })
    }

    pub async fn get(&self, key: &SessionKey) -> Result<Option<Session>, GatewayError> {
        key.validate()?;
        self.storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await
    }

    /// Acquire the binding and all concurrency permits for one request.
    ///
    /// Missing, expired, invalid or account-unavailable bindings fail with a
    /// classified error; nothing is created, moved or deleted here.
    pub async fn acquire(&self, key: &SessionKey) -> Result<SessionLease, GatewayError> {
        key.validate()?;
        let session = self
            .storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await?
            .ok_or_else(|| binding_error("no session binding exists for this client session"))?;

        if session.state == SessionState::Invalid {
            return Err(binding_error(format!(
                "session binding is invalid: {}",
                session
                    .invalid_reason
                    .as_deref()
                    .unwrap_or("no reason recorded")
            )));
        }
        if session.expires_at <= Utc::now() {
            return Err(binding_error(
                "session binding expired; rebind it explicitly (the account and upstream id are kept until then)",
            ));
        }

        let account = self
            .accounts
            .get(&session.account_id)
            .await?
            .ok_or_else(|| binding_error("the bound account no longer exists"))?;
        if account.kind != key.kind || account.region != key.region {
            return Err(binding_error(
                "the bound account does not match the session provider/region",
            ));
        }
        if !account.enabled {
            return Err(GatewayError::new(
                ErrorKind::AccountDisabled,
                format!(
                    "account {} is disabled; the session binding is kept, not moved",
                    account.id
                ),
            ));
        }
        if let Some(error) = auth_error(account.auth_status, &account.id) {
            return Err(error);
        }

        let deadline = tokio::time::Instant::now() + self.policy.queue_timeout;
        let session_guard = if self.policy.serialize_session {
            Some(self.gate.acquire(&key.gate_key(), deadline).await?)
        } else {
            None
        };
        let operator_limit = usize::try_from(account.max_concurrency).unwrap_or(1);
        let account_lease = self
            .limiter
            .acquire(&account.id, operator_limit, deadline)
            .await?;

        Ok(SessionLease {
            storage: self.storage.clone(),
            key: key.clone(),
            session,
            ttl: self.ttl,
            _session_guard: session_guard,
            _account_lease: account_lease,
        })
    }

    /// Record the upstream session id returned by the provider. Fails when the
    /// caller observes a stale account or revision, so an id can never be
    /// attached to the wrong account by a racing request.
    pub async fn attach_upstream(
        &self,
        key: &SessionKey,
        account_id: &str,
        expected_revision: i64,
        upstream_session_id: &str,
    ) -> Result<Session, GatewayError> {
        key.validate()?;
        if upstream_session_id.trim().is_empty() {
            return Err(GatewayError::bad_request(
                "upstream session id must not be empty",
            ));
        }
        let updated = self
            .storage
            .sessions()
            .set_upstream(
                &key.client_session_id,
                key.kind,
                key.region,
                account_id,
                expected_revision,
                upstream_session_id.trim(),
                self.ttl.as_secs(),
            )
            .await?;
        updated.ok_or_else(|| {
            binding_error(
                "session binding changed concurrently (account or revision mismatch); upstream id not attached",
            )
        })
    }

    /// Mark a binding invalid without deleting it. Idempotent when already
    /// invalid.
    pub async fn invalidate(
        &self,
        key: &SessionKey,
        reason: &str,
    ) -> Result<Session, GatewayError> {
        key.validate()?;
        let current = self
            .storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await?
            .ok_or_else(|| binding_error("no session binding exists for this client session"))?;
        if current.state == SessionState::Invalid {
            return Ok(current);
        }
        let reason = sanitize_reason(reason);
        let updated = self
            .storage
            .sessions()
            .invalidate(
                &key.client_session_id,
                key.kind,
                key.region,
                current.binding_revision,
                &reason,
            )
            .await?;
        updated.ok_or_else(|| {
            binding_error("session binding changed concurrently; reload before invalidating")
        })
    }

    /// Recovery for an upstream session the same account can no longer use: the
    /// account is kept, the upstream id is dropped, and the reason is recorded.
    /// The adapter must resend the full history next time.
    pub async fn reset_upstream(
        &self,
        key: &SessionKey,
        reason: &str,
    ) -> Result<Session, GatewayError> {
        key.validate()?;
        let current = self
            .storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await?
            .ok_or_else(|| binding_error("no session binding exists for this client session"))?;
        let reason = sanitize_reason(reason);
        let updated = self
            .storage
            .sessions()
            .reset_upstream(
                &key.client_session_id,
                key.kind,
                key.region,
                &current.account_id,
                current.binding_revision,
                &reason,
                self.ttl.as_secs(),
            )
            .await?;
        updated.ok_or_else(|| {
            binding_error("session binding changed concurrently; reload before resetting upstream")
        })
    }

    /// Explicitly move a binding to another account. The upstream session id is
    /// cleared, never carried across accounts.
    pub async fn rebind(
        &self,
        key: &SessionKey,
        account_id: &str,
    ) -> Result<Session, GatewayError> {
        key.validate()?;
        let current = self
            .storage
            .sessions()
            .get(&key.client_session_id, key.kind, key.region)
            .await?
            .ok_or_else(|| binding_error("no session binding exists for this client session"))?;
        let account = self.account_for(key, account_id).await?;
        let updated = self
            .storage
            .sessions()
            .rebind(
                &key.client_session_id,
                key.kind,
                key.region,
                current.binding_revision,
                &account.id,
                &account.provider,
                None,
                self.ttl.as_secs(),
            )
            .await?;
        updated.ok_or_else(|| {
            binding_error("session binding changed concurrently; reload before rebinding")
        })
    }

    /// Apply one explicit recovery strategy.
    pub async fn recover(
        &self,
        key: &SessionKey,
        recovery: SessionRecovery,
    ) -> Result<Session, GatewayError> {
        match recovery {
            SessionRecovery::ResetUpstream { reason } => self.reset_upstream(key, &reason).await,
            SessionRecovery::Rebind { account_id } => self.rebind(key, &account_id).await,
        }
    }

    async fn account_for(
        &self,
        key: &SessionKey,
        account_id: &str,
    ) -> Result<crate::accounts::Account, GatewayError> {
        let account = self
            .accounts
            .get(account_id)
            .await?
            .ok_or_else(|| GatewayError::bad_request("account not found"))?;
        if account.kind != key.kind || account.region != key.region {
            return Err(GatewayError::bad_request(
                "account provider/region does not match the session binding",
            ));
        }
        Ok(account)
    }
}

fn binding_error(message: impl Into<String>) -> GatewayError {
    GatewayError::new(ErrorKind::SessionBindingInvalid, message)
}

fn auth_error(status: AuthStatus, account_id: &str) -> Option<GatewayError> {
    match status {
        AuthStatus::Unknown | AuthStatus::Valid => None,
        AuthStatus::Expired | AuthStatus::RefreshRequired => Some(GatewayError::new(
            ErrorKind::AuthExpired,
            format!("account {account_id} credential must be refreshed before use"),
        )),
        AuthStatus::Invalid => Some(GatewayError::new(
            ErrorKind::AuthInvalid,
            format!("account {account_id} credential is invalid"),
        )),
        AuthStatus::Restricted => Some(GatewayError::new(
            ErrorKind::ChannelRestricted,
            format!("account {account_id} is restricted upstream"),
        )),
    }
}

fn sanitize_reason(reason: &str) -> String {
    let text = redact_secrets(reason);
    if text.chars().count() <= MAX_REASON_CHARS {
        return text;
    }
    let mut out: String = text.chars().take(MAX_REASON_CHARS).collect();
    out.push('…');
    out
}
