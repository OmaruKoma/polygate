//! Quota groups and snapshots.
//!
//! Quota is *never* invented. If a provider cannot report a balance, the status
//! is `unknown` and the numeric fields stay `None` — the UI must render
//! "Unknown", not 0 or infinity.
//!
//! Snapshot units are platform specific (`credits`, `requests`, ...). Platform
//! credits are not tokens unless a verified conversion is configured; this
//! module keeps the unit explicit so that consumers cannot silently mix them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::accounts::Account;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaUnit {
    Credits,
    Requests,
    Tokens,
    Unknown,
}

impl QuotaUnit {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuotaUnit::Credits => "credits",
            QuotaUnit::Requests => "requests",
            QuotaUnit::Tokens => "tokens",
            QuotaUnit::Unknown => "unknown",
        }
    }
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "credits" => QuotaUnit::Credits,
            "requests" => QuotaUnit::Requests,
            "tokens" => QuotaUnit::Tokens,
            _ => QuotaUnit::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    /// Balance was fetched from upstream.
    Known,
    /// Provider does not expose a balance for this account.
    Unknown,
    /// The last refresh attempt failed.
    Error,
    /// Some group members are unknown; aggregate values only cover known ones.
    Partial,
}

impl QuotaStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuotaStatus::Known => "known",
            QuotaStatus::Unknown => "unknown",
            QuotaStatus::Error => "error",
            QuotaStatus::Partial => "partial",
        }
    }
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "known" => QuotaStatus::Known,
            "error" => QuotaStatus::Error,
            "partial" => QuotaStatus::Partial,
            _ => QuotaStatus::Unknown,
        }
    }
}

/// One balance observation for an account (optionally per model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub account_id: String,
    pub group_id: Option<String>,
    pub model: Option<String>,
    pub unit: QuotaUnit,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub status: QuotaStatus,
    pub error: Option<String>,
    pub refreshed_at: DateTime<Utc>,
    pub raw: Option<serde_json::Value>,
}

impl QuotaSnapshot {
    pub fn unknown(account_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            group_id: None,
            model: None,
            unit: QuotaUnit::Unknown,
            total: None,
            remaining: None,
            status: QuotaStatus::Unknown,
            error: Some(reason.into()),
            refreshed_at: Utc::now(),
            raw: None,
        }
    }

    pub fn error(account_id: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut snap = Self::unknown(account_id, reason);
        snap.status = QuotaStatus::Error;
        snap
    }

    pub fn ratio(&self) -> Option<f64> {
        match (self.remaining, self.total) {
            (Some(r), Some(t)) if t > 0.0 => Some((r / t).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    pub fn exhausted(&self) -> bool {
        matches!(self.status, QuotaStatus::Known)
            && self.remaining.map(|r| r <= 0.0).unwrap_or(false)
    }
}

/// Grouped view for the admin API.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaGroupView {
    pub id: String,
    pub name: String,
    pub shared: bool,
    pub accounts: Vec<String>,
    pub providers: Vec<String>,
    pub unit: QuotaUnit,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub status: QuotaStatus,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub members: Vec<QuotaMemberView>,
    pub models: Vec<QuotaModelView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaMemberView {
    pub account_id: String,
    pub account_name: String,
    pub provider: String,
    pub unit: QuotaUnit,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub status: QuotaStatus,
    pub error: Option<String>,
    pub refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaModelView {
    pub model: String,
    pub unit: QuotaUnit,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub status: QuotaStatus,
}

/// In-memory latest snapshot index; history lives in SQLite.
#[derive(Default)]
pub struct QuotaIndex {
    latest: HashMap<String, QuotaSnapshot>,
    per_model: HashMap<(String, String), QuotaSnapshot>,
}

impl QuotaIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, snap: QuotaSnapshot) {
        if let Some(model) = &snap.model {
            self.per_model
                .insert((snap.account_id.clone(), model.clone()), snap.clone());
        } else {
            self.latest.insert(snap.account_id.clone(), snap);
        }
    }

    pub fn get(&self, account_id: &str) -> Option<&QuotaSnapshot> {
        self.latest.get(account_id)
    }

    pub fn snapshot_for(&self, account_id: &str) -> Option<&QuotaSnapshot> {
        self.latest.get(account_id)
    }

    pub fn is_exhausted(&self, account_id: &str, min_ratio: f64) -> bool {
        match self.latest.get(account_id) {
            Some(s) if s.exhausted() => true,
            Some(s) => s.ratio().map(|r| r < min_ratio).unwrap_or(false),
            None => false, // unknown is never treated as exhausted
        }
    }

    pub fn build_group(
        &self,
        id: &str,
        name: &str,
        shared: bool,
        accounts: &[Account],
    ) -> QuotaGroupView {
        let mut members = Vec::new();
        let mut known_total: Option<f64> = None;
        let mut known_remaining: Option<f64> = None;
        let mut units = std::collections::HashSet::new();
        let mut unknowns = 0usize;
        let mut errors = 0usize;
        let mut latest: Option<DateTime<Utc>> = None;
        let mut providers = std::collections::BTreeSet::new();
        let mut model_map: HashMap<String, QuotaSnapshot> = HashMap::new();

        for acc in accounts {
            providers.insert(acc.provider.clone());
            let snap = self.latest.get(&acc.id);
            if let Some(s) = snap {
                latest = Some(
                    latest
                        .map(|l| l.max(s.refreshed_at))
                        .unwrap_or(s.refreshed_at),
                );
            }
            members.push(QuotaMemberView {
                account_id: acc.id.clone(),
                account_name: acc.name.clone(),
                provider: acc.provider.clone(),
                unit: snap.map(|s| s.unit).unwrap_or(QuotaUnit::Unknown),
                total: snap.and_then(|s| s.total),
                remaining: snap.and_then(|s| s.remaining),
                status: snap.map(|s| s.status).unwrap_or(QuotaStatus::Unknown),
                error: snap.and_then(|s| s.error.clone()),
                refreshed_at: snap.map(|s| s.refreshed_at),
            });
            match snap.map(|s| s.status).unwrap_or(QuotaStatus::Unknown) {
                QuotaStatus::Known => {
                    if let Some(s) = snap {
                        units.insert(s.unit);
                        if let Some(t) = s.total {
                            known_total = Some(known_total.unwrap_or(0.0) + t);
                        }
                        if let Some(r) = s.remaining {
                            known_remaining = Some(known_remaining.unwrap_or(0.0) + r);
                        }
                    }
                }
                QuotaStatus::Error => errors += 1,
                _ => unknowns += 1,
            }
            for ((acc_id, model), s) in &self.per_model {
                if acc_id == &acc.id && s.status == QuotaStatus::Known {
                    let entry = model_map
                        .entry(model.clone())
                        .or_insert_with(|| QuotaSnapshot {
                            account_id: String::new(),
                            group_id: Some(id.to_string()),
                            model: Some(model.clone()),
                            unit: s.unit,
                            total: Some(0.0),
                            remaining: Some(0.0),
                            status: QuotaStatus::Known,
                            error: None,
                            refreshed_at: s.refreshed_at,
                            raw: None,
                        });
                    entry.total = Some(entry.total.unwrap_or(0.0) + s.total.unwrap_or(0.0));
                    entry.remaining =
                        Some(entry.remaining.unwrap_or(0.0) + s.remaining.unwrap_or(0.0));
                }
            }
        }

        // For a shared quota pool every member reports the same balance; summing
        // would double count. The latest known snapshot is used instead.
        if shared {
            if let Some(most_recent) = members
                .iter()
                .filter(|m| m.status == QuotaStatus::Known)
                .max_by_key(|m| m.refreshed_at)
            {
                known_total = most_recent.total;
                known_remaining = most_recent.remaining;
                unknowns = 0;
            }
        }

        let status = if members.is_empty() {
            QuotaStatus::Unknown
        } else if errors == members.len() {
            QuotaStatus::Error
        } else if unknowns > 0 && known_remaining.is_some() {
            QuotaStatus::Partial
        } else if known_remaining.is_some() {
            QuotaStatus::Known
        } else if errors > 0 {
            QuotaStatus::Error
        } else {
            QuotaStatus::Unknown
        };

        let unit = if units.len() == 1 {
            *units.iter().next().unwrap()
        } else {
            QuotaUnit::Unknown
        };

        QuotaGroupView {
            id: id.to_string(),
            name: name.to_string(),
            shared,
            accounts: accounts.iter().map(|a| a.id.clone()).collect(),
            providers: providers.into_iter().collect(),
            unit,
            total: known_total,
            remaining: known_remaining,
            status,
            refreshed_at: latest,
            members,
            models: model_map
                .into_values()
                .map(|s| QuotaModelView {
                    model: s.model.unwrap_or_default(),
                    unit: s.unit,
                    total: s.total,
                    remaining: s.remaining,
                    status: s.status,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{ProviderKind, Region};

    use super::*;

    fn account(id: &str) -> Account {
        Account {
            id: id.into(),
            provider: "workbuddy-cn".into(),
            kind: ProviderKind::Workbuddy,
            region: Region::Cn,
            name: id.into(),
            credential_hint: String::new(),
            external_id: None,
            enabled: true,
            weight: 1,
            max_concurrency: 1,
            quota_group: Some("tencent-a".into()),
            auth_status: crate::accounts::AuthStatus::Valid,
            auth_checked_at: None,
            last_error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn unknown_never_becomes_zero() {
        let idx = QuotaIndex::new();
        let g = idx.build_group("g", "G", false, &[account("a")]);
        assert_eq!(g.status, QuotaStatus::Unknown);
        assert!(g.remaining.is_none());
    }

    #[test]
    fn shared_group_does_not_double_count() {
        let mut idx = QuotaIndex::new();
        for id in ["a", "b"] {
            idx.update(QuotaSnapshot {
                account_id: id.into(),
                group_id: Some("g".into()),
                model: None,
                unit: QuotaUnit::Credits,
                total: Some(100.0),
                remaining: Some(50.0),
                status: QuotaStatus::Known,
                error: None,
                refreshed_at: Utc::now(),
                raw: None,
            });
        }
        let g = idx.build_group("g", "G", true, &[account("a"), account("b")]);
        assert_eq!(g.remaining, Some(50.0));
        let independent = idx.build_group("g", "G", false, &[account("a"), account("b")]);
        assert_eq!(independent.remaining, Some(100.0));
    }
}
