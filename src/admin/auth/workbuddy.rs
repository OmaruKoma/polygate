//! WorkBuddy device-authorization flow lifecycle (CN and Intl).
//!
//! WorkBuddy has no browser callback: the administrator opens the official
//! `authUrl` returned by `/v2/plugin/auth/state` and the status endpoint polls
//! `/v2/plugin/auth/token`. The upstream issues no separate short user code, so
//! the real authorization URL is the only thing the operator is shown.
//!
//! The upstream `state` is required for every poll and is stored encrypted with
//! the flow id as AAD so the database never holds it in plaintext. It is also
//! embedded in the official `authUrl` the operator must open; that URL is the
//! only place it is exposed and it is never accepted from the browser. Flow
//! polling is serialized per flow, transient upstream failures leave the flow
//! pending, and a rejected or expired authorization is terminal.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::workbuddy::auth::WorkbuddyPollOutcome;
use crate::storage::AuthFlowRow;

use crate::admin::account::persist_workbuddy_login;
use crate::admin::AdminState;

use super::flow::{flow_aad, view, FlowView};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbuddyFlowSecrets {
    pub upstream_state: String,
    /// Upstream authorization deadline, Unix seconds; `0` means unknown.
    pub expires_at: i64,
}

impl AdminState {
    pub async fn start_workbuddy_flow(
        &self,
        region: Region,
        name: Option<String>,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        if self.storage.admin().active_flow(now).await?.is_some() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "another authorization is already in progress; cancel it or wait for it to expire",
            ));
        }
        let start = self.workbuddy_auth.start(region).await?;
        let flow_id = uuid::Uuid::new_v4().to_string();
        let secrets = WorkbuddyFlowSecrets {
            upstream_state: start.upstream_state,
            expires_at: start.expires_at,
        };
        let encoded = serde_json::to_vec(&secrets)
            .map_err(|_| GatewayError::internal("cannot encode authorization secrets"))?;
        let encrypted = self
            .secret_box
            .encrypt_with_aad(&flow_aad(&flow_id), &encoded)
            .map_err(|_| GatewayError::internal("cannot protect authorization secrets"))?;
        let ttl = now
            + chrono::Duration::from_std(self.settings.flow_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let upstream_deadline =
            chrono::DateTime::from_timestamp(start.expires_at, 0).unwrap_or(ttl);
        let expires_at = ttl.min(upstream_deadline);
        let requested_name = name
            .map(|value| value.trim().chars().take(64).collect::<String>())
            .filter(|value| !value.is_empty());
        let flow = AuthFlowRow {
            id: flow_id,
            kind: "workbuddy".into(),
            region: region.as_str().into(),
            status: "pending".into(),
            authorize_url: start.auth_url,
            // Device authorization has no callback URL; the column is NOT NULL
            // but has no CHECK, so an empty string is the honest value.
            redirect_uri: String::new(),
            ticket_id: uuid::Uuid::new_v4().to_string(),
            secrets_encrypted: encrypted,
            requested_name,
            account_id: None,
            error: None,
            created_at: now,
            expires_at,
            completed_at: None,
        };
        if let Err(error) = self.storage.admin().create_flow(&flow).await {
            if self.storage.admin().active_flow(now).await?.is_some() {
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "another authorization is already in progress",
                ));
            }
            return Err(error);
        }
        Ok(view(&flow))
    }

    /// Poll once while pending, then persist the account. Token values never
    /// enter the returned view.
    pub async fn workbuddy_flow_status(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let mut flow = self.workbuddy_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }

        let _guard = self.workbuddy_flow_locks.lock(&flow.id).await;
        // Another status request may have completed the flow while we waited.
        flow = self.workbuddy_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }
        // The UI may poll faster than the upstream interval; a throttled poll
        // is a no-op that keeps the flow pending.
        if !self.workbuddy_poll_guard.allow(&flow.id) {
            return Ok(view(&flow));
        }

        let region = Region::from_db_str(&flow.region).ok_or_else(|| {
            GatewayError::internal("stored authorization flow has an unknown region")
        })?;
        let secrets = match decrypt_workbuddy_secrets(self, &flow) {
            Ok(secrets) => secrets,
            Err(error) => {
                let _ = self
                    .storage
                    .admin()
                    .finish_flow(&flow.id, "failed", None, Some(&error.message))
                    .await;
                return Err(error);
            }
        };

        match self
            .workbuddy_auth
            .poll(
                region,
                &secrets.upstream_state,
                secrets.expires_at,
                Utc::now().timestamp(),
            )
            .await
        {
            Ok(WorkbuddyPollOutcome::Pending) => Ok(view(&flow)),
            Ok(WorkbuddyPollOutcome::Completed(credential)) => {
                let account = match persist_workbuddy_login(
                    self,
                    region,
                    *credential,
                    flow.requested_name.as_deref(),
                )
                .await
                {
                    Ok(account) => account,
                    Err(error) => {
                        let _ = self
                            .storage
                            .admin()
                            .finish_flow(&flow.id, "failed", None, Some(&error.message))
                            .await;
                        return Err(error);
                    }
                };
                self.storage
                    .admin()
                    .finish_flow(&flow.id, "succeeded", Some(&account.id), None)
                    .await?;
                Ok(FlowView {
                    flow_id: flow.id,
                    status: "succeeded".into(),
                    region: flow.region,
                    authorize_url: flow.authorize_url,
                    created_at: flow.created_at,
                    expires_at: flow.expires_at,
                    account_id: Some(account.id),
                    error: None,
                })
            }
            Err(error) => {
                if is_terminal(&error) {
                    let _ = self
                        .storage
                        .admin()
                        .finish_flow(&flow.id, "failed", None, Some(&error.message))
                        .await;
                }
                Err(error)
            }
        }
    }

    pub async fn cancel_workbuddy_flow(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        self.storage.admin().expire_flows(Utc::now()).await?;
        let flow = self.workbuddy_flow(flow_id).await?;
        let _ = self.storage.admin().cancel_flow(&flow.id).await?;
        let flow = self.workbuddy_flow(flow_id).await?;
        Ok(view(&flow))
    }

    async fn workbuddy_flow(&self, flow_id: &str) -> Result<AuthFlowRow, GatewayError> {
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        if flow.kind != "workbuddy" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is not a WorkBuddy flow",
            ));
        }
        Ok(flow)
    }
}

fn decrypt_workbuddy_secrets(
    state: &AdminState,
    flow: &AuthFlowRow,
) -> Result<WorkbuddyFlowSecrets, GatewayError> {
    let mut plaintext = state
        .secret_box
        .decrypt_with_aad(&flow_aad(&flow.id), &flow.secrets_encrypted)
        .map_err(|_| GatewayError::internal("stored authorization secrets cannot be opened"))?;
    let parsed = serde_json::from_slice::<WorkbuddyFlowSecrets>(&plaintext)
        .map_err(|_| GatewayError::internal("stored authorization secrets are invalid"));
    plaintext.zeroize();
    parsed
}

/// Keep the flow pending for conditions a later poll can recover from.
fn is_terminal(error: &GatewayError) -> bool {
    !matches!(
        error.kind,
        ErrorKind::Timeout
            | ErrorKind::Transport
            | ErrorKind::UpstreamServer
            | ErrorKind::RateLimited
            | ErrorKind::NoCapacity
    )
}
