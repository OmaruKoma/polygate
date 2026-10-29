//! Unified model registry.
//!
//! A model exposed to downstream clients is a *route*: unified id -> provider
//! instance -> upstream raw id. Capabilities determine which APIs can serve it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{ModelCapabilitiesConfig, ModelConfig, ProviderConfig, Region};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub chat: bool,
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
}

impl From<ModelCapabilitiesConfig> for ModelCapabilities {
    fn from(c: ModelCapabilitiesConfig) -> Self {
        Self {
            chat: c.chat,
            streaming: c.streaming,
            tools: c.tools,
            vision: c.vision,
            reasoning: c.reasoning,
        }
    }
}

impl ModelCapabilities {
    pub fn openai_list_item(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.chat {
            out.push("chat");
        }
        if self.tools {
            out.push("tools");
        }
        if self.vision {
            out.push("vision");
        }
        if self.reasoning {
            out.push("reasoning");
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub id: String,
    pub provider: String,
    pub provider_name: String,
    pub kind: crate::config::ProviderKind,
    pub region: Region,
    pub raw_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub capabilities: ModelCapabilities,
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
    pub aliases: Vec<String>,
    pub source: String,
    pub updated_at: DateTime<Utc>,
}

impl Model {
    pub fn from_config(p: &ProviderConfig, m: &ModelConfig) -> Self {
        Self {
            id: m.id.clone(),
            provider: p.provider_id(),
            provider_name: p.display_name(),
            kind: p.kind,
            region: p.region,
            raw_id: m.raw_id.clone().unwrap_or_else(|| m.id.clone()),
            display_name: m.display_name.clone().unwrap_or_else(|| m.id.clone()),
            enabled: m.enabled && p.enabled,
            capabilities: m.capabilities.into(),
            context_window: m.context_window,
            max_output: m.max_output,
            aliases: m.aliases.clone(),
            source: "config".into(),
            updated_at: Utc::now(),
        }
    }

    /// OpenAI `/v1/models` representation.
    pub fn openai_object(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "object": "model",
            "created": self.updated_at.timestamp(),
            "owned_by": self.provider,
            "display_name": self.display_name,
            "capabilities": self.capabilities.openai_list_item(),
            "context_window": self.context_window,
            "provider": self.provider,
            "region": self.region.as_str(),
            "raw_id": self.raw_id,
        })
    }

    /// Anthropic `/v1/models` representation.
    pub fn anthropic_object(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "model",
            "id": self.id,
            "display_name": self.display_name,
            "created_at": self.updated_at.to_rfc3339(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    models: Vec<Model>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_providers(providers: &[ProviderConfig]) -> Self {
        let mut models = Vec::new();
        for p in providers {
            for m in &p.models {
                models.push(Model::from_config(p, m));
            }
        }
        Self { models }
    }

    pub fn add(&mut self, model: Model) {
        if let Some(existing) = self
            .models
            .iter_mut()
            .find(|m| m.id == model.id && m.provider == model.provider && m.region == model.region)
        {
            *existing = model;
        } else {
            self.models.push(model);
        }
    }

    pub fn set_provider_enabled(&mut self, provider: &str, enabled: bool) {
        for m in &mut self.models {
            if m.provider == provider {
                m.enabled = enabled;
            }
        }
    }

    pub fn all(&self) -> &[Model] {
        &self.models
    }

    /// Resolve by unified id or alias, preferring enabled models and the given
    /// provider instance when specified.
    pub fn resolve(&self, name: &str, provider: Option<&str>) -> Option<Model> {
        self.models
            .iter()
            .filter(|m| m.enabled)
            .filter(|m| provider.map(|p| m.provider == p).unwrap_or(true))
            .find(|m| m.id == name)
            .or_else(|| {
                self.models
                    .iter()
                    .filter(|m| m.enabled)
                    .filter(|m| provider.map(|p| m.provider == p).unwrap_or(true))
                    .find(|m| m.aliases.iter().any(|a| a == name) || m.raw_id == name)
            })
            .cloned()
    }

    pub fn for_provider(&self, provider: &str) -> Vec<&Model> {
        self.models
            .iter()
            .filter(|m| m.provider == provider)
            .collect()
    }

    pub fn replace(&mut self, models: Vec<Model>) {
        self.models = models;
    }
}
