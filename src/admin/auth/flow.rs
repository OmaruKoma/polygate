//! CodeArts authorization flow lifecycle.
//!
//! The upstream callback contains only `code`, so one flow may be live at a
//! time. PKCE verifier and the per-flow DPoP private key are stored encrypted;
//! they are never returned by the status endpoint.

use chrono::Utc;
use serde::Serialize;
use zeroize::Zeroize;

use crate::config::Region;
use crate::error::{ErrorKind, GatewayError};
use crate::providers::codearts::login::{self, LoginSecrets};
use crate::storage::AuthFlowRow;

use crate::admin::account::persist_codearts_login;
use crate::admin::AdminState;

#[derive(Debug, Clone, Serialize)]
pub struct FlowView {
    pub flow_id: String,
    pub status: String,
    pub region: String,
    pub authorize_url: String,
    pub created_at: chrono::DateTime<Utc>,
    pub expires_at: chrono::DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AdminState {
    pub async fn start_codearts_flow(
        &self,
        region: Region,
        name: Option<String>,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        self.ensure_no_active_flow(now).await?;
        let port = url::Url::parse(&self.settings.redirect_url)
            .ok()
            .and_then(|url| url.port_or_known_default())
            .unwrap_or(8787);
        let start = login::begin(
            &self.settings.codearts,
            region,
            &self.settings.redirect_url,
            port,
            now,
        )?;
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
            id: flow_id.clone(),
            kind: "codearts".into(),
            region: region.as_str().into(),
            status: "pending".into(),
            authorize_url: start.authorize_url,
            redirect_uri: self.settings.redirect_url.clone(),
            ticket_id: start.ticket_id,
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

    pub async fn flow_status(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        Ok(view(&flow))
    }

    pub async fn cancel_codearts_flow(&self, flow_id: &str) -> Result<FlowView, GatewayError> {
        let _ = self.storage.admin().cancel_flow(flow_id).await?;
        self.flow_status(flow_id).await
    }

    /// Reject a new login while another one holds the single slot, naming it.
    pub(super) async fn ensure_no_active_flow(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<(), GatewayError> {
        let Some(flow) = self.storage.admin().active_flow(now).await? else {
            return Ok(());
        };
        Err(GatewayError::new(
            ErrorKind::BadRequest,
            format!(
                "another authorization is already in progress ({} {}, started {}); cancel it or wait for it to expire",
                flow.kind,
                flow.region,
                flow.created_at.to_rfc3339()
            ),
        ))
    }

    /// Cancel whichever flow currently holds the single slot, if any.
    pub async fn cancel_active_flow(&self) -> Result<Option<FlowView>, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let Some(flow) = self.storage.admin().active_flow(now).await? else {
            return Ok(None);
        };
        self.storage.admin().cancel_flow(&flow.id).await?;
        Ok(Some(self.flow_status(&flow.id).await?))
    }

    /// Called by `GET /oauth/callback`. There is exactly one live flow.
    pub async fn complete_active_flow(&self, code: &str) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let flow = self
            .storage
            .admin()
            .active_flow(now)
            .await?
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::BadRequest,
                    "no pending CodeArts authorization; start a new login",
                )
            })?;
        self.finish_codearts_flow(flow, code, now).await
    }

    /// Complete a named pending flow from a pasted callback URL.
    ///
    /// CodeArts binds the authorization code to a loopback callback, so a
    /// browser on another machine never reaches the gateway. The operator
    /// copies the address bar instead and the exchange happens here.
    pub async fn complete_codearts_flow(
        &self,
        flow_id: &str,
        raw: &str,
    ) -> Result<FlowView, GatewayError> {
        let now = Utc::now();
        self.storage.admin().expire_flows(now).await?;
        let flow = self.storage.admin().flow(flow_id).await?.ok_or_else(|| {
            GatewayError::new(ErrorKind::BadRequest, "authorization flow not found")
        })?;
        if flow.kind != "codearts" {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is not a CodeArts login",
            ));
        }
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
        let code = code_from_callback(raw)?;
        self.finish_codearts_flow(flow, &code, now).await
    }

    async fn finish_codearts_flow(
        &self,
        flow: AuthFlowRow,
        code: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<FlowView, GatewayError> {
        if !self.storage.admin().claim_flow(&flow.id, now).await? {
            return Err(GatewayError::new(
                ErrorKind::BadRequest,
                "authorization flow is no longer pending",
            ));
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
        let region = Region::from_db_str(&flow.region).ok_or_else(|| {
            GatewayError::internal("stored authorization flow has an unknown region")
        })?;
        let outcome = login::exchange(
            &self.http,
            &self.settings.codearts,
            &flow.redirect_uri,
            code,
            &secrets,
            now,
        )
        .await;
        drop(secrets);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(flow = %flow.id, error = %error.message, "CodeArts authorization failed");
                let _ = self
                    .storage
                    .admin()
                    .finish_flow(&flow.id, "failed", None, Some(&error.message))
                    .await;
                return Err(error);
            }
        };
        let account =
            match persist_codearts_login(self, region, outcome, flow.requested_name.as_deref())
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

    pub async fn fail_active_flow(&self, reason: &str) -> Option<FlowView> {
        let now = Utc::now();
        let _ = self.storage.admin().expire_flows(now).await;
        let Ok(Some(flow)) = self.storage.admin().active_flow(now).await else {
            return None;
        };
        let _ = self
            .storage
            .admin()
            .finish_flow(&flow.id, "failed", None, Some(reason))
            .await;
        Some(FlowView {
            flow_id: flow.id,
            status: "failed".into(),
            region: flow.region,
            authorize_url: flow.authorize_url,
            created_at: flow.created_at,
            expires_at: flow.expires_at,
            account_id: None,
            error: Some(reason.into()),
        })
    }
}

fn decrypt_secrets(state: &AdminState, flow: &AuthFlowRow) -> Result<LoginSecrets, GatewayError> {
    let mut plaintext = state
        .secret_box
        .decrypt_with_aad(&flow_aad(&flow.id), &flow.secrets_encrypted)
        .map_err(|_| GatewayError::internal("stored authorization secrets cannot be opened"))?;
    let parsed = serde_json::from_slice::<LoginSecrets>(&plaintext)
        .map_err(|_| GatewayError::internal("stored authorization secrets are invalid"));
    plaintext.zeroize();
    parsed
}

pub(super) fn view(flow: &AuthFlowRow) -> FlowView {
    let status = if matches!(flow.status.as_str(), "pending" | "exchanging")
        && flow.expires_at <= Utc::now()
    {
        "expired".to_string()
    } else {
        flow.status.clone()
    };
    FlowView {
        flow_id: flow.id.clone(),
        status,
        region: flow.region.clone(),
        authorize_url: flow.authorize_url.clone(),
        created_at: flow.created_at,
        expires_at: flow.expires_at,
        account_id: flow.account_id.clone(),
        error: flow.error.clone(),
    }
}

/// Accept either a full callback URL or a bare code from the operator.
fn code_from_callback(raw: &str) -> Result<String, GatewayError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(GatewayError::new(
            ErrorKind::BadRequest,
            "paste the callback URL from the browser address bar",
        ));
    }
    let candidate = if raw.starts_with("http://") || raw.starts_with("https://") {
        let url = url::Url::parse(raw).map_err(|_| {
            GatewayError::new(
                ErrorKind::BadRequest,
                "the pasted callback URL is not valid",
            )
        })?;
        url.query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .ok_or_else(|| {
                GatewayError::new(
                    ErrorKind::BadRequest,
                    "the pasted URL has no `code` parameter",
                )
            })?
    } else {
        raw.to_string()
    };
    let candidate = candidate.trim().to_string();
    if candidate.is_empty() || candidate.len() > 4096 {
        return Err(GatewayError::new(
            ErrorKind::BadRequest,
            "the pasted value is not a usable authorization code",
        ));
    }
    Ok(candidate)
}

pub(super) fn flow_aad(flow_id: &str) -> Vec<u8> {
    format!("polygate:admin-flow:v1:{flow_id}").into_bytes()
}
