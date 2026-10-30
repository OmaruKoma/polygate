//! Qoder device-authorization flow lifecycle.
//!
//! Qoder has no browser callback: the administrator authenticates on the
//! official page and the status endpoint polls the device-token endpoint. Flow
//! secrets (PKCE verifier, nonce, machine id) are encrypted with the flow id as
//! AAD and never returned to the browser.
//!
//! `GET /admin/api/auth/qoder/{flow_id}` is therefore not a pure read: while the
//! flow is pending it performs at most one upstream poll, serialized per flow.
//! Transient upstream failures leave the flow pending so the UI can retry;
//! authentication and request-shape failures are terminal.

use chrono::Utc;
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::qoder::auth::QoderAuthSecrets;
use crate::storage::AuthFlowRow;

use crate::admin::account::persist_qoder_login;
use crate::admin::AdminState;

use super::flow::{flow_aad, view, FlowView};

impl AdminState {
    pub async fn start_qoder_flow(
        &self,
        region: Region,
        name: Option<String>,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        self.ensure_no_active_flow(now).await?;
        let start = self.qoder_auth.start(region)?;
        let flow_id = uuid::Uuid::new_v4().to_string();
        let ticket_id = start.secrets.nonce.clone();
        let secrets = serde_json::to_vec(&start.secrets)
            .map_err(|_| GatewayError::internal("cannot encode authorization secrets"))?;
        let encrypted = self
            .secret_box
            .encrypt_with_aad(&flow_aad(&flow_id), &secrets)
            .map_err(|_| GatewayError::internal("cannot protect authorization secrets"))?;
        let expires_at = now
            + chrono::Duration::from_std(self.settings.flow_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let requested_name = name
            .map(|value| value.trim().chars().take(64).collect::<String>())
            .filter(|value| !value.is_empty());
        let flow = AuthFlowRow {
            id: flow_id,
            kind: "qoder".into(),
            region: region.as_str().into(),
            status: "pending".into(),
            authorize_url: start.authorize_url,
            // Device authorization has no callback URL. The column is NOT NULL
            // but has no CHECK; an empty string is the honest value.
            redirect_uri: String::new(),
            ticket_id,
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
    pub async fn qoder_flow_status(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let mut flow = self.qoder_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }

        let _guard = self.qoder_flow_locks.lock(&flow.id).await;
        // Another status request may have completed the flow while we waited.
        flow = self.qoder_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }

        let region = Region::from_db_str(&flow.region).ok_or_else(|| {
            GatewayError::internal("stored authorization flow has an unknown region")
        })?;
        let secrets = match decrypt_secrets(self, &flow) {
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

        match self.qoder_auth.poll(region, &secrets).await {
            Ok(crate::providers::qoder::auth::QoderPollOutcome::Pending) => Ok(view(&flow)),
            Ok(crate::providers::qoder::auth::QoderPollOutcome::Completed(credential)) => {
                let account = match persist_qoder_login(
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

    pub async fn cancel_qoder_flow(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        self.storage.admin().expire_flows(Utc::now()).await?;
        let flow = self.qoder_flow(flow_id).await?;
        let _ = self.storage.admin().cancel_flow(&flow.id).await?;
        let flow = self.qoder_flow(flow_id).await?;
        Ok(view(&flow))
    }

    async fn qoder_flow(&self, flow_id: &str) -> Result<AuthFlowRow, GatewayError> {
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        if flow.kind != "qoder" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is not a Qoder flow",
            ));
        }
        Ok(flow)
    }
}

fn decrypt_secrets(
    state: &AdminState,
    flow: &AuthFlowRow,
) -> Result<QoderAuthSecrets, GatewayError> {
    let mut plaintext = state
        .secret_box
        .decrypt_with_aad(&flow_aad(&flow.id), &flow.secrets_encrypted)
        .map_err(|_| GatewayError::internal("stored authorization secrets cannot be opened"))?;
    let parsed = serde_json::from_slice::<QoderAuthSecrets>(&plaintext)
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
