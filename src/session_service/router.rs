//! Minimal account routing on top of session bindings.
//!
//! The full scheduler (rotation, weights, quota-aware fairness) is M19. This
//! layer only does what the CodeArts adapter needs today:
//!
//! 1. an existing `(client session, provider kind, region)` binding wins;
//! 2. otherwise the first enabled, dispatchable account with a credential is
//!    bound — explicitly documented as "no load balancing yet";
//! 3. one upstream session id is generated per binding and reused verbatim;
//! 4. the account/session permits are held for the whole request (including the
//!    lifetime of an SSE stream);
//! 5. failures are classified and either reset the upstream session (same
//!    account) or update the account auth status. A busy account never causes a
//!    silent rebind.

use std::sync::{Arc, Mutex};

use crate::account_service::AccountService;
use crate::accounts::Credential;
use crate::error::{redact_secrets, ErrorKind, GatewayError};
use crate::providers::ProviderContext;
use crate::scheduler::Scheduler;

use super::service::{
    classify_session_failure, SessionBindingService, SessionFailure, SessionKey, SessionLease,
};

const MAX_REASON_CHARS: usize = 512;

#[derive(Clone)]
pub struct SessionRouter {
    sessions: SessionBindingService,
    accounts: AccountService,
    scheduler: Arc<Scheduler>,
}

impl SessionRouter {
    pub fn new(sessions: SessionBindingService, accounts: AccountService) -> Self {
        Self::with_scheduler(sessions, accounts, Arc::new(Scheduler::default()))
    }

    /// Attach the runtime scheduler (rotation, weights, quota and cooldown
    /// awareness). Without it the router still works but always picks the
    /// lowest account id.
    pub fn with_scheduler(
        sessions: SessionBindingService,
        accounts: AccountService,
        scheduler: Arc<Scheduler>,
    ) -> Self {
        Self {
            sessions,
            accounts,
            scheduler,
        }
    }

    pub fn sessions(&self) -> &SessionBindingService {
        &self.sessions
    }

    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// Resolve (creating if necessary) the binding for this key and acquire all
    /// concurrency permits. The returned lease must be kept until the upstream
    /// call — or the SSE body — is finished.
    pub async fn open(&self, key: SessionKey) -> Result<RouteLease, GatewayError> {
        self.open_excluding(key, &[]).await
    }

    /// Same as [`open`], but never uses one of the `excluded` accounts. Used to
    /// fail over before any output was produced; the existing binding is moved
    /// with a CAS rebind and its upstream session id is dropped (never carried
    /// across accounts).
    pub async fn open_excluding(
        &self,
        key: SessionKey,
        excluded: &[String],
    ) -> Result<RouteLease, GatewayError> {
        key_validate(&key)?;

        let mut binding = self.sessions.get(&key).await?;
        if let Some(session) = &binding {
            if excluded.contains(&session.account_id) {
                let account = self.select_account(&key, excluded).await?;
                self.sessions.rebind(&key, &account.id).await?;
                binding = self.sessions.get(&key).await?;
            }
        }

        let mut upstream = match binding {
            Some(session) => session.upstream_session_id,
            None => {
                let account = self.select_account(&key, excluded).await?;
                let generated = new_upstream_id();
                let outcome = self
                    .sessions
                    .bind_if_absent(&key, &account.id, Some(&generated))
                    .await?;
                if excluded.contains(&outcome.session.account_id) {
                    let account = self.select_account(&key, excluded).await?;
                    self.sessions.rebind(&key, &account.id).await?;
                    self.sessions
                        .get(&key)
                        .await?
                        .and_then(|session| session.upstream_session_id)
                } else {
                    outcome.session.upstream_session_id
                }
            }
        };

        let lease = self.sessions.acquire(&key).await?;
        if upstream.as_deref().map(str::is_empty).unwrap_or(true) {
            let generated = upstream.unwrap_or_else(new_upstream_id);
            self.sessions
                .attach_upstream(
                    &key,
                    lease.account_id(),
                    lease.session().binding_revision,
                    &generated,
                )
                .await?;
            upstream = Some(generated);
        }

        let account_id = lease.account_id().to_string();
        let authorization = self
            .accounts
            .authorization_context_for(&account_id, key.kind, key.region)
            .await?;
        let credential = authorization.credential.ok_or_else(|| {
            GatewayError::new(
                ErrorKind::AuthInvalid,
                format!("account {account_id} has no stored credential"),
            )
        })?;

        self.scheduler.acquired(&account_id);
        Ok(RouteLease {
            key,
            lease,
            account_id,
            credential,
            credential_revision: authorization.credential_revision,
            upstream_session_id: upstream.unwrap_or_default(),
            failure: Mutex::new(None),
        })
    }

    /// After a provider error: reset the upstream session for the same account,
    /// update auth status for credential failures, or do nothing for transient
    /// failures. Recovery errors are logged by the caller; they never replace
    /// the original provider error.
    pub async fn report_failure(&self, lease: &RouteLease, error: &GatewayError) {
        if let Ok(mut slot) = lease.failure.lock() {
            *slot = Some(error.kind);
        }
        match classify_session_failure(error) {
            SessionFailure::UpstreamSessionInvalid => {
                let reason = sanitize(&error.message);
                if let Err(recovery) = self.sessions.reset_upstream(&lease.key, &reason).await {
                    tracing::warn!(
                        account = %lease.account_id,
                        error = %recovery.message,
                        "could not reset the upstream session after an invalid-session error"
                    );
                }
            }
            SessionFailure::AccountUnavailable => {
                if let Err(recovery) = self
                    .accounts
                    .mark_auth_failure(&lease.account_id, error)
                    .await
                {
                    tracing::warn!(
                        account = %lease.account_id,
                        error = %recovery.message,
                        "could not persist the auth status after an upstream rejection"
                    );
                }
            }
            SessionFailure::AccountBusy | SessionFailure::Other => {}
        }
    }

    /// Persist the request count/TTL and release the permits.
    pub async fn finish(&self, lease: RouteLease) {
        let failure = lease.failure.lock().ok().and_then(|slot| *slot);
        self.scheduler.release(&lease.account_id, failure);
        if let Err(error) = lease.lease.finish().await {
            tracing::warn!(
                error = %error.message,
                "could not finish the session lease"
            );
        }
    }

    async fn select_account(
        &self,
        key: &SessionKey,
        excluded: &[String],
    ) -> Result<crate::accounts::Account, GatewayError> {
        let mut candidates = self.accounts.list_available(key.kind, key.region).await?;
        candidates.retain(|account| !excluded.contains(&account.id));
        self.scheduler
            .select(key.kind, key.region, &candidates)
            .await
    }
}

/// One account + credential + upstream session, with permits held.
pub struct RouteLease {
    key: SessionKey,
    lease: SessionLease,
    account_id: String,
    credential: Credential,
    credential_revision: Option<i64>,
    upstream_session_id: String,
    /// Last error observed for this lease; consumed by `finish` so the
    /// scheduler sees one outcome per request.
    failure: Mutex<Option<ErrorKind>>,
}

impl std::fmt::Debug for RouteLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteLease")
            .field("account_id", &self.account_id)
            .field("client_session_id", &self.key.client_session_id)
            .field("upstream_session_id", &self.upstream_session_id)
            .field("credential_revision", &self.credential_revision)
            .finish_non_exhaustive()
    }
}

impl RouteLease {
    pub fn key(&self) -> &SessionKey {
        &self.key
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn upstream_session_id(&self) -> &str {
        &self.upstream_session_id
    }

    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    pub fn provider_context(&self) -> ProviderContext {
        ProviderContext {
            account_id: Some(self.account_id.clone()),
            credential_revision: self.credential_revision,
            client_session_id: Some(self.key.client_session_id.clone()),
            upstream_session_id: Some(self.upstream_session_id.clone()),
        }
    }
}

fn key_validate(key: &SessionKey) -> Result<(), GatewayError> {
    if key.client_session_id.trim().is_empty() {
        return Err(GatewayError::bad_request("client session id is empty"));
    }
    Ok(())
}

fn new_upstream_id() -> String {
    // 32 lowercase hex, matching the upstream client's chat_id format.
    uuid::Uuid::new_v4().simple().to_string()
}

fn sanitize(message: &str) -> String {
    let text = redact_secrets(message);
    if text.chars().count() <= MAX_REASON_CHARS {
        return text;
    }
    let mut out: String = text.chars().take(MAX_REASON_CHARS).collect();
    out.push('…');
    out
}
