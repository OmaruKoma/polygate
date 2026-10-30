//! Account views and the CodeArts authorization -> account persistence path.

use serde_json::{json, Value};

use crate::account_service::{AccountUpdate, CreateAccount};
use crate::accounts::Account;
use crate::config::{ProviderKind, Region};
use crate::error::GatewayError;
use crate::providers::codearts::login::LoginOutcome;
use crate::providers::qoder::credential::QoderCredential;

use super::AdminState;

/// Persist a successful browser authorization.
///
/// Identity is `codearts:<region>:<principal_id>`; re-authorizing the same
/// upstream user rotates that row's credential instead of creating a copy.
/// New accounts and their encrypted credential are written in one transaction.
pub async fn persist_codearts_login(
    state: &AdminState,
    region: Region,
    outcome: LoginOutcome,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let LoginOutcome {
        credential,
        principal_id,
        name,
        metadata,
        ..
    } = outcome;
    let external_id = format!("codearts:{}:{principal_id}", region.as_str());
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>());

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Codearts, region, &external_id)
        .await?
    {
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &credential, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &credential)
                    .await?;
            }
        }
        let mut merged = existing.metadata.as_object().cloned().unwrap_or_default();
        if let Some(extra) = metadata.as_object() {
            for (key, value) in extra {
                merged.insert(key.clone(), value.clone());
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name.unwrap_or_else(|| existing.name.clone())),
                    metadata: Some(Value::Object(merged)),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Codearts,
        region,
        requested_name.unwrap_or(name),
        Some(credential.to_json().clone()),
    );
    request.provider = format!("codearts-{}", region.as_str());
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

/// Persist a successful Qoder device authorization.
///
/// Identity is `qoder:<region>:<uid>`; re-authorizing the same upstream user
/// rotates that row's credential instead of creating a copy. The token JSON is
/// only written through `CredentialService` (SecretBox + revision CAS).
pub async fn persist_qoder_login(
    state: &AdminState,
    region: Region,
    credential: QoderCredential,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let external_id = format!("qoder:{}:{}", region.as_str(), credential.uid);
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| credential.name_hint());
    let metadata = json!({
        "uid": credential.uid,
        "nickname": credential.nickname,
        "realm": region.as_str(),
        "domain": credential.domain,
        "user_type": credential.user_type,
        "organization_id": credential.organization_id,
        "organization_name": credential.organization_name,
        "auth_source": credential.auth_source,
    });

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Qoder, region, &external_id)
        .await?
    {
        let secret = credential_json(&credential);
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &secret, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &secret)
                    .await?;
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name),
                    metadata: Some(metadata),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Qoder,
        region,
        requested_name,
        Some(credential.to_json()),
    );
    request.provider = format!("qoder-{}", region.as_str());
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

fn credential_json(credential: &QoderCredential) -> crate::accounts::Credential {
    crate::accounts::Credential::from_value(credential.to_json())
}

/// Persist a successful WorkBuddy device authorization.
///
/// Identity is `workbuddy:<region>:<uid>`; re-authorizing the same upstream user
/// rotates that row's credential instead of creating a copy. Only normalized
/// fields are written (no device fingerprint, no desktop PII).
pub async fn persist_workbuddy_login(
    state: &AdminState,
    region: Region,
    credential: crate::providers::workbuddy::credential::WorkbuddyCredential,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let external_id = format!("workbuddy:{}:{}", region.as_str(), credential.uid);
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| credential.name_hint());
    let metadata = json!({
        "uid": credential.uid,
        "enterprise_id": credential.enterprise_id,
        "realm": region.as_str(),
        "domain": credential.domain,
        "auth_source": "oauth",
    });
    let secret = crate::accounts::Credential::from_value(credential.to_json());

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Workbuddy, region, &external_id)
        .await?
    {
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &secret, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &secret)
                    .await?;
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name),
                    metadata: Some(metadata),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Workbuddy,
        region,
        requested_name,
        Some(credential.to_json()),
    );
    request.provider = format!("workbuddy-{}", region.as_str());
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

pub async fn persist_trae_login(
    state: &AdminState,
    region: Region,
    credential: crate::providers::trae::credential::TraeCredential,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let external_id = format!("trae:{}:{}", region.as_str(), credential.uid);
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| credential.name_hint());
    let metadata = json!({
        "uid": credential.uid,
        "enterprise_id": credential.enterprise_id,
        "nickname": credential.nickname,
        "realm": region.as_str(),
        "domain": credential.domain,
        "auth_source": "oauth",
    });
    let secret = crate::accounts::Credential::from_value(credential.to_json());

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Trae, region, &external_id)
        .await?
    {
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &secret, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &secret)
                    .await?;
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name),
                    metadata: Some(metadata),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Trae,
        region,
        requested_name,
        Some(credential.to_json()),
    );
    request.provider = format!("trae-{}", region.as_str());
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

/// Non-secret account view used by every admin endpoint.
pub async fn view(state: &AdminState, account: &Account) -> Value {
    let credential = state
        .accounts
        .credentials()
        .status(&account.id)
        .await
        .ok()
        .flatten();
    let quota = state
        .storage
        .quota()
        .latest_for_account(&account.id)
        .await
        .ok()
        .flatten();
    let last_checkin = state
        .checkin
        .history(&account.id, 1)
        .await
        .ok()
        .and_then(|mut runs| runs.drain(..).next())
        .map(|run| {
            json!({
                "business_date": run.business_date,
                "status": run.status.as_str(),
                "summary": run.summary,
                "completed_at": run.completed_at,
                "error_category": run.error_category.map(|value| value.as_str()),
            })
        });

    json!({
        "id": account.id,
        "provider": account.provider,
        "kind": account.kind.as_str(),
        "region": account.region.as_str(),
        "name": account.name,
        "external_id": account.external_id,
        "enabled": account.enabled,
        "weight": account.weight,
        "max_concurrency": account.max_concurrency,
        "quota_group": account.quota_group,
        "auth_status": account.auth_status.as_str(),
        "auth_checked_at": account.auth_checked_at,
        "last_error": account.last_error,
        "credential": {
            "present": credential.as_ref().map(|value| value.present).unwrap_or(false),
            "revision": credential.as_ref().and_then(|value| value.revision),
            "updated_at": credential.as_ref().and_then(|value| value.updated_at),
            "encryption_version": credential.as_ref().and_then(|value| value.encryption_version.clone()),
        },
        "quota": quota,
        "last_checkin": last_checkin,
        "created_at": account.created_at,
        "updated_at": account.updated_at,
        "metadata": account.metadata,
    })
}

/// Persist an imported Loomy session.
///
/// Loomy has no OAuth: the operator copies the 14-day login session out of the
/// desktop client (or its `auth-session.json`) and imports it here. Nothing but
/// the session, the user id and an advisory expiry is stored — no phone number,
/// no password, no device identity.
///
/// Identity is `loomy:cn:<userid>` when the operator supplies one, so a renewed
/// session rotates that row's credential. Without a user id the identity falls
/// back to a short hash of the session, which means a *new* session imports as
/// a separate account.
pub async fn persist_loomy_session(
    state: &AdminState,
    session: String,
    userid: Option<String>,
    expires_at: i64,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let region = Region::Cn;
    let userid = userid
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let external_id = match &userid {
        Some(uid) => format!("loomy:cn:{uid}"),
        None => format!("loomy:cn:session-{}", session_fingerprint(&session)),
    };
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| match &userid {
            Some(uid) => format!("Loomy {uid}"),
            None => "Loomy session".to_string(),
        });
    let metadata = json!({
        "userid": userid.clone().unwrap_or_default(),
        "realm": region.as_str(),
        "auth_source": "session-import",
        "expires_at": expires_at,
    });
    let secret = crate::accounts::Credential::from_value(json!({
        "session": session,
        "userid": userid.clone().unwrap_or_default(),
        "expires_at": expires_at,
    }));

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Loomy, region, &external_id)
        .await?
    {
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &secret, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &secret)
                    .await?;
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name),
                    metadata: Some(metadata),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Loomy,
        region,
        requested_name,
        Some(secret.to_json().clone()),
    );
    request.provider = "loomy-cn".to_string();
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

/// Persist a successful OfficeAce authorization.
///
/// Identity is `officeace:cn:<account_id>`; when the control plane does not
/// report one, a fingerprint of the app key keeps re-authorizations on the same
/// row instead of creating a duplicate.
pub async fn persist_officeace_login(
    state: &AdminState,
    credential: Value,
    account_id: &str,
    nickname: &str,
    requested_name: Option<&str>,
) -> Result<Account, GatewayError> {
    let region = Region::Cn;
    let account_id = account_id.trim();
    let app_key = credential
        .get("model_app_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let external_id = if account_id.is_empty() {
        format!("officeace:cn:app-{}", session_fingerprint(app_key))
    } else {
        format!("officeace:cn:{account_id}")
    };
    let nickname = nickname.trim();
    let requested_name = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| {
            if nickname.is_empty() {
                "OfficeAce account".to_string()
            } else {
                format!("OfficeAce {nickname}")
            }
        });
    let metadata = json!({
        "account_id": account_id,
        "principal_id": credential.get("principal_id").cloned().unwrap_or(Value::Null),
        "user_name": credential.get("user_name").cloned().unwrap_or(Value::Null),
        "realm": region.as_str(),
        "auth_source": "oauth",
    });
    let secret = crate::accounts::Credential::from_value(credential);

    if let Some(existing) = state
        .accounts
        .find_by_external(ProviderKind::Officeace, region, &external_id)
        .await?
    {
        let status = state.accounts.credentials().status(&existing.id).await?;
        match status.and_then(|status| status.revision) {
            Some(revision) => {
                state
                    .accounts
                    .credentials()
                    .update(&existing.id, &secret, revision)
                    .await?;
            }
            None => {
                state
                    .accounts
                    .credentials()
                    .save(&existing.id, &secret)
                    .await?;
            }
        }
        let updated = state
            .accounts
            .update(
                &existing.id,
                AccountUpdate {
                    name: Some(requested_name),
                    metadata: Some(metadata),
                    ..AccountUpdate::default()
                },
            )
            .await?;
        state
            .accounts
            .record_auth_status(&existing.id, crate::accounts::AuthStatus::Valid, None)
            .await?;
        return Ok(updated);
    }

    let mut request = CreateAccount::new(
        ProviderKind::Officeace,
        region,
        requested_name,
        Some(secret.to_json().clone()),
    );
    request.provider = "officeace-cn".to_string();
    request.external_id = Some(external_id);
    request.metadata = metadata;
    let account = state.accounts.create(request).await?;
    state
        .accounts
        .record_auth_status(&account.id, crate::accounts::AuthStatus::Valid, None)
        .await?;
    Ok(account)
}

/// Stable, non-reversible fingerprint used only for an account identity.
fn session_fingerprint(session: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
