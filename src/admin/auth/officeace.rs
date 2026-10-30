//! OfficeAce authorization flow lifecycle.
//!
//! OfficeAce uses the **poll** shape: the callback is handled by the Huawei
//! cloud and the gateway asks `/v1/claw/auth/code` for the code. A poll is not
//! read-only, so it is serialized per flow and the flow holds the single active
//! slot shared with every other provider's login.
//!
//! `switch_account` routes the browser through the cloud logout page first;
//! without it a live Huawei session is reused silently and the operator cannot
//! choose which account is being added.

use chrono::Utc;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::officeace::http::OfficeAceHttp;
use crate::providers::officeace::login::{self, LoginSecrets};
use crate::storage::AuthFlowRow;

use crate::admin::account::persist_officeace_login;
use crate::admin::AdminState;

use super::flow::{flow_aad, view, FlowView};

impl AdminState {
    pub async fn start_officeace_flow(
        &self,
        name: Option<String>,
        switch_account: bool,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        self.ensure_no_active_flow(now).await?;
        let mut config = self.settings.officeace.clone();
        config.switch_account = switch_account;
        let http = self.officeace_http();
        let start = login::begin(&http, &config).await?;
        let flow_id = uuid::Uuid::new_v4().to_string();
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
            kind: "officeace".into(),
            region: Region::Cn.as_str().into(),
            status: "pending".into(),
            authorize_url: start.authorize_url,
            redirect_uri: config.redirect_uri(),
            ticket_id: start.secrets.state.clone(),
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

    /// Poll once while pending, then persist the account. Secrets never leave
    /// this function.
    pub async fn officeace_flow_status(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let mut flow = self.officeace_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }

        let _guard = self.officeace_flow_locks.lock(&flow.id).await;
        flow = self.officeace_flow(flow_id).await?;
        if flow.status != "pending" {
            return Ok(view(&flow));
        }

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
        let http = self.officeace_http();
        match login::poll(&http, &self.settings.officeace, &secrets).await {
            Ok(login::PollOutcome::Pending) => Ok(view(&flow)),
            Ok(login::PollOutcome::Completed(outcome)) => {
                let account = match persist_officeace_login(
                    self,
                    outcome.credential,
                    &outcome.account_id,
                    &outcome.nickname,
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
                // A flow error is not an authentication failure: `AuthInvalid`
                // maps to HTTP 401, which the panel would treat as "your admin
                // session expired" and kick the operator back to the login
                // page. Persist the failure and report it as a flow state.
                if is_terminal(&error) {
                    let _ = self
                        .storage
                        .admin()
                        .finish_flow(&flow.id, "failed", None, Some(&error.message))
                        .await;
                    let flow = self.officeace_flow(flow_id).await?;
                    return Ok(view(&flow));
                }
                Err(error)
            }
        }
    }

    pub async fn cancel_officeace_flow(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        self.storage.admin().expire_flows(Utc::now()).await?;
        let flow = self.officeace_flow(flow_id).await?;
        let _ = self.storage.admin().cancel_flow(&flow.id).await?;
        let flow = self.officeace_flow(flow_id).await?;
        Ok(view(&flow))
    }

    fn officeace_http(&self) -> OfficeAceHttp {
        OfficeAceHttp::with_endpoints(
            self.http.clone(),
            self.settings.officeace.cloud.clone(),
            self.settings.officeace.sts.clone(),
        )
    }

    async fn officeace_flow(&self, flow_id: &str) -> Result<AuthFlowRow, GatewayError> {
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        if flow.kind != "officeace" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is not an OfficeAce flow",
            ));
        }
        Ok(flow)
    }
}

fn decrypt_secrets(state: &AdminState, flow: &AuthFlowRow) -> Result<LoginSecrets, GatewayError> {
    use zeroize::Zeroize;
    let mut plaintext = state
        .secret_box
        .decrypt_with_aad(&flow_aad(&flow.id), &flow.secrets_encrypted)
        .map_err(|_| GatewayError::internal("stored authorization secrets cannot be opened"))?;
    let parsed = serde_json::from_slice::<LoginSecrets>(&plaintext)
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
