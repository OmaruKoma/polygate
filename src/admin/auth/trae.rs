//! TRAE callback authorization flow (CN only).
//!
//! TRAE redirects the browser to `auth_callback_url` with the refresh token in
//! the query string. The server never receives a password and never follows the
//! callback; the administrator copies the address bar back into the panel
//! (`POST /admin/api/auth/trae/{flow_id}/complete`), which is the same paste
//! route Coding2API documents for deployments where the callback is not
//! reachable (`admin_auth.py:124-160`, `355-370`).
//!
//! The machine/device identity is **operator-supplied**. The reference generates
//! a fresh hex32 pair per login; PolyGate refuses to fabricate device identities
//! and therefore requires the pair at flow start. It is stored encrypted with
//! the flow and persisted with the credential so refresh stays consistent.
//!
//! One flow at a time is a global admin constraint (the callback URL carries no
//! flow id of its own). The pasted URL is bound to the flow through its
//! `loginTraceID` when present.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::storage::AuthFlowRow;

use crate::admin::account::persist_trae_login;
use crate::admin::AdminState;

use super::flow::{flow_aad, view, FlowView};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraeFlowSecrets {
    pub machine_id: String,
    pub device_id: String,
}

impl AdminState {
    pub async fn start_trae_flow(
        &self,
        region: Region,
        name: Option<String>,
        machine_id: Option<String>,
        device_id: Option<String>,
    ) -> Result<FlowView, GatewayError> {
        let machine_id = machine_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let device_id = device_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let (Some(machine_id), Some(device_id)) = (machine_id, device_id) else {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "TRAE login needs the operator-supplied machineId and deviceId \
                 (32 hex characters each); PolyGate does not generate device identities",
            ));
        };
        if !is_hex32(&machine_id) || !is_hex32(&device_id) {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "machineId and deviceId must be 32 hexadecimal characters",
            ));
        }

        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        if self.storage.admin().active_flow(now).await?.is_some() {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "another authorization is already in progress; cancel it or wait for it to expire",
            ));
        }
        let start =
            self.trae_auth
                .start(region, &machine_id, &device_id, &self.settings.redirect_url)?;
        let flow_id = uuid::Uuid::new_v4().to_string();
        let secrets = TraeFlowSecrets {
            machine_id,
            device_id,
        };
        let encoded = serde_json::to_vec(&secrets)
            .map_err(|_| GatewayError::internal("cannot encode authorization secrets"))?;
        let encrypted = self
            .secret_box
            .encrypt_with_aad(&flow_aad(&flow_id), &encoded)
            .map_err(|_| GatewayError::internal("cannot protect authorization secrets"))?;
        let expires_at = now
            + chrono::Duration::from_std(self.settings.flow_ttl)
                .unwrap_or_else(|_| chrono::Duration::minutes(10));
        let requested_name = name
            .map(|value| value.trim().chars().take(64).collect::<String>())
            .filter(|value| !value.is_empty());
        let flow = AuthFlowRow {
            id: flow_id,
            kind: "trae".into(),
            region: region.as_str().into(),
            status: "pending".into(),
            authorize_url: start.auth_url,
            redirect_uri: self.settings.redirect_url.clone(),
            ticket_id: start.login_trace_id,
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

    /// Complete a pending flow from the callback URL the browser ended on.
    /// Token values never enter the returned view.
    pub async fn complete_trae_flow(
        &self,
        flow_id: &str,
        raw_url: &str,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let mut flow = self.trae_flow(flow_id).await?;
        if flow.status != "pending" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is no longer pending",
            ));
        }
        if flow.expires_at <= now {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow has expired; start a new login",
            ));
        }

        let _guard = self.trae_flow_locks.lock(&flow.id).await;
        flow = self.trae_flow(flow_id).await?;
        if flow.status != "pending" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is no longer pending",
            ));
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
        if let Some(trace) = callback_trace(raw_url) {
            if !trace.is_empty() && trace != flow.ticket_id {
                // A callback from another login attempt: keep this flow pending
                // so the operator can paste the right one.
                return Err(GatewayError::new(
                    ErrorKind::BadRequest,
                    "the pasted callback does not belong to this login; start a new one",
                ));
            }
        }

        match self
            .trae_auth
            .complete(region, raw_url, &secrets.machine_id, &secrets.device_id)
            .await
        {
            Ok(credential) => {
                let account = match persist_trae_login(
                    self,
                    region,
                    credential,
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

    pub async fn trae_flow_status(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        self.storage.admin().expire_flows(Utc::now()).await?;
        let flow = self.trae_flow(flow_id).await?;
        Ok(view(&flow))
    }

    pub async fn cancel_trae_flow(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        self.storage.admin().expire_flows(Utc::now()).await?;
        let flow = self.trae_flow(flow_id).await?;
        let _ = self.storage.admin().cancel_flow(&flow.id).await?;
        let flow = self.trae_flow(flow_id).await?;
        Ok(view(&flow))
    }

    async fn trae_flow(&self, flow_id: &str) -> Result<AuthFlowRow, GatewayError> {
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        if flow.kind != "trae" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is not a TRAE flow",
            ));
        }
        Ok(flow)
    }
}

fn decrypt_secrets(
    state: &AdminState,
    flow: &AuthFlowRow,
) -> Result<TraeFlowSecrets, GatewayError> {
    let mut plaintext = state
        .secret_box
        .decrypt_with_aad(&flow_aad(&flow.id), &flow.secrets_encrypted)
        .map_err(|_| GatewayError::internal("stored authorization secrets cannot be opened"))?;
    let parsed = serde_json::from_slice::<TraeFlowSecrets>(&plaintext)
        .map_err(|_| GatewayError::internal("stored authorization secrets are invalid"));
    plaintext.zeroize();
    parsed
}

fn callback_trace(raw_url: &str) -> Option<String> {
    let url = url::Url::parse(raw_url.trim()).ok()?;
    url.query_pairs()
        .find(|(key, _)| key == "loginTraceID" || key == "login_trace_id")
        .map(|(_, value)| value.into_owned())
}

fn is_hex32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Keep the flow pending for conditions a retry can recover from.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_must_be_hex32() {
        assert!(is_hex32("0123456789abcdef0123456789abcdef"));
        assert!(!is_hex32("0123456789abcdef"));
        assert!(!is_hex32("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn callback_trace_is_read_from_the_query() {
        assert_eq!(
            callback_trace("http://x/cb?loginTraceID=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(callback_trace("not a url"), None);
        assert_eq!(callback_trace("http://x/cb").as_deref(), None);
    }
}
