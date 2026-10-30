//! Configuration loading: TOML file + `POLYGATE_*` environment overrides.
//!
//! Configuration is intentionally boring: no remote config, no hot reload of
//! security-sensitive values. Provider entries are declarative so operators can
//! enable/disable each provider and region independently.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::GatewayError;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub scheduler: SchedulerConfig,
    pub metrics: MetricsConfig,
    pub logging: LoggingConfig,
    pub admin: AdminConfig,
    pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Bind address. Loopback by default: exposing the gateway publicly must be
    /// an explicit operator decision.
    pub listen: String,
    /// Absolute public URL used in documentation links / cookies.
    pub public_url: Option<String>,
    /// Honour `X-Forwarded-For` / `X-Forwarded-Proto` (only behind a trusted proxy).
    pub trust_proxy: bool,
    pub request_timeout_secs: u64,
    pub max_body_bytes: usize,
    /// Allowed CORS origins for the web UI / API. Empty = same-origin only.
    pub cors_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8787".into(),
            public_url: None,
            trust_proxy: false,
            request_timeout_secs: 600,
            max_body_bytes: 16 * 1024 * 1024,
            cors_origins: Vec::new(),
        }
    }
}

impl ServerConfig {
    pub fn socket_addr(&self) -> Result<SocketAddr, GatewayError> {
        self.listen.parse().map_err(|e| {
            GatewayError::config(format!("invalid listen address {}: {e}", self.listen))
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    /// Flush aggregated usage events every N seconds (0 = every request).
    pub usage_flush_secs: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://data/polygate.db?mode=rwc".into(),
            // SQLite serializes writes; a small pool is enough and keeps RSS low.
            max_connections: 4,
            usage_flush_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Gateway API keys (preferred over `bootstrap_api_key`). Keys must be at
    /// least 16 characters and must not be example placeholders.
    pub api_keys: Vec<String>,
    /// Extra bootstrap gateway key (env `POLYGATE_API_KEY`). Keys created through
    /// the admin API are stored hashed in SQLite.
    pub bootstrap_api_key: Option<String>,
    /// Admin token. When absent a random one is generated and stored (hashed) in
    /// the database; the plaintext is printed once to the console on first boot.
    pub admin_token: Option<String>,
    pub admin_session_ttl_hours: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerStrategy {
    /// Smooth weighted round-robin across eligible accounts (default).
    #[default]
    SmoothWeightedRoundRobin,
    RoundRobin,
    LeastConnections,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SchedulerConfig {
    pub strategy: SchedulerStrategy,
    /// Session affinity TTL. Expired sessions are garbage collected.
    pub session_ttl_secs: u64,
    /// How long a request may wait for account/provider capacity.
    pub queue_timeout_secs: u64,
    /// Provider-wide concurrency ceiling used for admission control.
    pub provider_max_concurrency: usize,
    pub cooldown_base_secs: u64,
    pub cooldown_max_secs: u64,
    /// Consecutive failures before an account is cooled down.
    pub failure_threshold: u32,
    /// Exclude an account from scheduling when known remaining quota ratio is
    /// below this value. Unknown quota never excludes an account.
    pub quota_min_ratio: f64,
    /// Maximum number of sessions kept in the in-memory cache.
    pub session_cache_capacity: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            strategy: SchedulerStrategy::SmoothWeightedRoundRobin,
            session_ttl_secs: 24 * 3600,
            queue_timeout_secs: 30,
            provider_max_concurrency: 64,
            cooldown_base_secs: 30,
            cooldown_max_secs: 600,
            failure_threshold: 3,
            quota_min_ratio: 0.0,
            session_cache_capacity: 10_000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    /// Include per-request rows in `GET /admin/logs`.
    pub request_logs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` filter directive: `trace`, `debug`, `info`, `warn`, `error`
    /// (optional `target=level` directives are also accepted).
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

impl LoggingConfig {
    /// Validate a filter directive without installing a subscriber.
    pub fn validate(&self) -> Result<(), GatewayError> {
        let level = self.level.trim();
        if level.is_empty() {
            return Err(GatewayError::config("logging.level must not be empty"));
        }
        tracing_subscriber::EnvFilter::try_new(level)
            .map_err(|e| GatewayError::config(format!("invalid logging.level: {e}")))?;
        Ok(())
    }
}

/// Region of an upstream platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Cn,
    Intl,
}

impl Region {
    pub fn as_str(&self) -> &'static str {
        match self {
            Region::Cn => "cn",
            Region::Intl => "intl",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "cn" => Some(Region::Cn),
            "intl" => Some(Region::Intl),
            _ => None,
        }
    }
}

impl std::fmt::Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Supported upstream platforms. `Mock` is a development/testing provider and
/// is never advertised as a verified real provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Codearts,
    Qoder,
    Trae,
    Workbuddy,
    Mock,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Codearts => "codearts",
            ProviderKind::Qoder => "qoder",
            ProviderKind::Trae => "trae",
            ProviderKind::Workbuddy => "workbuddy",
            ProviderKind::Mock => "mock",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "codearts" => Some(ProviderKind::Codearts),
            "qoder" => Some(ProviderKind::Qoder),
            "trae" => Some(ProviderKind::Trae),
            "workbuddy" => Some(ProviderKind::Workbuddy),
            "mock" => Some(ProviderKind::Mock),
            _ => None,
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            ProviderKind::Codearts => "Huawei CodeArts",
            ProviderKind::Qoder => "Alibaba Qoder",
            ProviderKind::Trae => "ByteDance TRAE",
            ProviderKind::Workbuddy => "Tencent WorkBuddy",
            ProviderKind::Mock => "Mock (development)",
        }
    }
}

/// Verification status, reported to clients and the admin UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verification {
    /// Tested against the real upstream with real credentials.
    Verified,
    /// Implemented from protocol research/source analysis, not fully verified.
    Experimental,
    /// Known not to be supported by the platform or by this implementation.
    Unsupported,
    /// Could not be validated (no credentials/access).
    Unavailable,
    /// Local deterministic test provider.
    Mock,
}

impl Verification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verification::Verified => "verified",
            Verification::Experimental => "experimental",
            Verification::Unsupported => "unsupported",
            Verification::Unavailable => "unavailable",
            Verification::Mock => "mock",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Unique provider instance id. Defaults to `<kind>-<region>`.
    #[serde(default)]
    pub id: Option<String>,
    pub kind: ProviderKind,
    pub region: Region,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub name: Option<String>,
    /// Override for the upstream base URL. Do not assume CN and INTL share one.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Shared quota pool identifier; accounts in the same group share credits.
    #[serde(default)]
    pub quota_group: Option<String>,
    #[serde(default)]
    pub max_concurrency: Option<usize>,
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Provider specific options (paths, client version, headers, ...).
    #[serde(default)]
    pub options: HashMap<String, String>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

fn default_true() -> bool {
    true
}

impl ProviderConfig {
    pub fn provider_id(&self) -> String {
        self.id
            .clone()
            .unwrap_or_else(|| format!("{}-{}", self.kind.as_str(), self.region.as_str()))
    }
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            format!(
                "{} ({})",
                self.kind.display_name(),
                self.region.as_str().to_uppercase()
            )
        })
    }
    pub fn verification(&self) -> Verification {
        self.verification.unwrap_or(match self.kind {
            ProviderKind::Mock => Verification::Mock,
            _ => Verification::Experimental,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    /// Unified model name exposed to downstream clients.
    pub id: String,
    /// Upstream model id. Defaults to `id`.
    #[serde(default)]
    pub raw_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_caps")]
    pub capabilities: ModelCapabilitiesConfig,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

fn default_caps() -> ModelCapabilitiesConfig {
    ModelCapabilitiesConfig {
        chat: true,
        streaming: true,
        tools: true,
        vision: false,
        reasoning: false,
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelCapabilitiesConfig {
    pub chat: bool,
    pub streaming: bool,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
}

impl Default for ModelCapabilitiesConfig {
    fn default() -> Self {
        default_caps()
    }
}

/// Web admin panel configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AdminConfig {
    pub enabled: bool,
    /// Public callback URL for the CodeArts browser authorization. When absent
    /// the gateway derives `http://127.0.0.1:<listener-port>/oauth/callback`,
    /// which only works when the browser runs on the same host; set it to the
    /// address the browser actually uses when listening on a LAN address.
    pub redirect_url: Option<String>,
    /// Reject the first-run setup unless the TCP peer is loopback.
    pub setup_local_only: bool,
    /// Force `Secure` cookies; otherwise it follows the request scheme.
    pub secure_cookie: Option<bool>,
    /// Extra allowed origins for mutating admin requests.
    pub allowed_origins: Vec<String>,
    pub flow_ttl_secs: u64,
    pub login_max_failures: u32,
    pub login_window_secs: u64,
    pub codearts_client_id: String,
    pub codearts_uri_scheme: String,
    pub codearts_theme: String,
    pub codearts_plugin_name: String,
    pub codearts_plugin_version: String,
    pub codearts_locale: String,
    pub codearts_cn_portal_url: String,
    pub codearts_intl_portal_url: String,
    pub codearts_sts_url: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            redirect_url: None,
            setup_local_only: true,
            secure_cookie: None,
            allowed_origins: Vec::new(),
            flow_ttl_secs: 600,
            login_max_failures: 5,
            login_window_secs: 300,
            codearts_client_id: "codearts-snap".into(),
            codearts_uri_scheme: "codearts-snap".into(),
            codearts_theme: "2".into(),
            codearts_plugin_name: "codearts-snap_vscode".into(),
            codearts_plugin_version: "26.7.2".into(),
            codearts_locale: "zh-cn".into(),
            codearts_cn_portal_url: "https://codearts.huaweicloud.com".into(),
            codearts_intl_portal_url: "https://codearts.ap-southeast-1.huaweicloud.com".into(),
            codearts_sts_url: "https://sts.cn-north-4.myhuaweicloud.com".into(),
        }
    }
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub config_path: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub scheduler: SchedulerConfig,
    pub metrics: MetricsConfig,
    pub logging: LoggingConfig,
    pub admin: AdminConfig,
    pub providers: Vec<ProviderConfig>,
}

/// Values that must never be accepted as gateway keys: they are the examples
/// shipped in documentation and are trivially guessable.
const PLACEHOLDER_KEYS: &[&str] = &[
    "change-me",
    "changeme",
    "example-key",
    "replace-me",
    "your-api-key",
    "test-key",
    "dev-key",
    "polygate-dev-key",
];

const MIN_API_KEY_LEN: usize = 16;

impl Config {
    /// Load config from TOML (if present) plus environment variables.
    pub fn load(
        config_path: Option<&Path>,
        data_dir: Option<PathBuf>,
    ) -> Result<Self, GatewayError> {
        Self::load_with_env(config_path, data_dir, |key| std::env::var(key).ok())
    }

    /// Same as [`Config::load`] with an explicit environment lookup, so
    /// precedence can be tested without mutating the process environment.
    pub fn load_with_env<F>(
        config_path: Option<&Path>,
        data_dir: Option<PathBuf>,
        env: F,
    ) -> Result<Self, GatewayError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let data_dir = data_dir.unwrap_or_else(|| PathBuf::from("data"));
        let config_path = config_path
            .map(Path::to_path_buf)
            .or_else(|| env("POLYGATE_CONFIG").map(PathBuf::from))
            .or_else(|| {
                let candidate = data_dir.join("config.toml");
                candidate.exists().then_some(candidate)
            });

        let mut file_cfg = FileConfig::default();
        if let Some(path) = &config_path {
            if path.exists() {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    GatewayError::config(format!("cannot read config {}: {e}", path.display()))
                })?;
                file_cfg = toml::from_str(&raw).map_err(|e| {
                    GatewayError::config(format!("invalid config {}: {e}", path.display()))
                })?;
            }
        }

        // Environment overrides (never log secrets).
        if let Some(v) = env("POLYGATE_LISTEN") {
            file_cfg.server.listen = v;
        }
        if let Some(v) = env("POLYGATE_PUBLIC_URL") {
            file_cfg.server.public_url = Some(v);
        }
        if let Some(v) = env("POLYGATE_DATABASE_URL") {
            file_cfg.database.url = v;
        }
        if let Some(v) = env("POLYGATE_ADMIN_TOKEN") {
            file_cfg.auth.admin_token = Some(v);
        }
        if let Some(v) = env("POLYGATE_API_KEY") {
            file_cfg.auth.bootstrap_api_key = Some(v);
        }
        if let Some(v) = env("POLYGATE_LOG_LEVEL") {
            file_cfg.logging.level = v;
        }
        let mock_flag = match env("POLYGATE_MOCK_ENABLED") {
            Some(value) => Some(match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                other => {
                    return Err(GatewayError::config(format!(
                        "invalid POLYGATE_MOCK_ENABLED value `{other}` (use true/false)"
                    )))
                }
            }),
            None => None,
        };
        if let Some(enabled) = mock_flag {
            for provider in &mut file_cfg.providers {
                if provider.kind == ProviderKind::Mock {
                    provider.enabled = enabled;
                }
            }
        }
        let mock_enabled = mock_flag.unwrap_or(true);
        if let Some(v) = env("POLYGATE_TRUST_PROXY") {
            file_cfg.server.trust_proxy = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("POLYGATE_SCHEDULER_STRATEGY") {
            let strategy = match v.as_str() {
                "round_robin" => SchedulerStrategy::RoundRobin,
                "least_connections" => SchedulerStrategy::LeastConnections,
                "smooth_weighted_round_robin" | "swrr" => {
                    SchedulerStrategy::SmoothWeightedRoundRobin
                }
                other => {
                    return Err(GatewayError::config(format!(
                        "unknown scheduler strategy {other}"
                    )))
                }
            };
            file_cfg.scheduler.strategy = strategy;
        }

        if file_cfg.providers.is_empty() {
            // Zero-config development default: a single mock provider, controlled
            // by `POLYGATE_MOCK_ENABLED`. Real providers are configured in
            // config.toml and are never enabled implicitly.
            file_cfg.providers.push(ProviderConfig {
                id: None,
                kind: ProviderKind::Mock,
                region: Region::Cn,
                enabled: mock_enabled,
                name: Some("Mock (development)".into()),
                base_url: None,
                quota_group: None,
                max_concurrency: None,
                verification: None,
                options: HashMap::new(),
                models: vec![
                    ModelConfig {
                        id: "mock-chat".into(),
                        raw_id: None,
                        display_name: Some("Mock Chat".into()),
                        enabled: true,
                        capabilities: default_caps(),
                        context_window: Some(128_000),
                        max_output: Some(8_192),
                        aliases: vec!["gpt-4o-mini".into()],
                    },
                    ModelConfig {
                        id: "mock-reasoner".into(),
                        raw_id: None,
                        display_name: Some("Mock Reasoner".into()),
                        enabled: true,
                        capabilities: ModelCapabilitiesConfig {
                            reasoning: true,
                            ..default_caps()
                        },
                        context_window: Some(128_000),
                        max_output: Some(8_192),
                        aliases: vec![],
                    },
                ],
            });
        }

        // Resolve data dir relative sqlite url.
        if file_cfg.database.url.starts_with("sqlite://data/") {
            let rel = file_cfg.database.url.trim_start_matches("sqlite://data/");
            let abs = data_dir.join(rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let url_path = abs.to_string_lossy().replace('\\', "/");
            file_cfg.database.url = format!("sqlite://{url_path}");
        }

        let mut cfg = Self {
            config_path,
            data_dir,
            server: file_cfg.server,
            database: file_cfg.database,
            auth: file_cfg.auth,
            scheduler: file_cfg.scheduler,
            metrics: file_cfg.metrics,
            logging: file_cfg.logging,
            admin: file_cfg.admin,
            providers: file_cfg.providers,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&mut self) -> Result<(), GatewayError> {
        self.server.socket_addr()?;
        self.logging.validate()?;
        self.validate_gateway_keys()?;
        if self.auth.admin_session_ttl_hours == 0 {
            self.auth.admin_session_ttl_hours = 24;
        }
        if self.admin.flow_ttl_secs == 0 {
            self.admin.flow_ttl_secs = 600;
        }
        if self.admin.login_max_failures == 0 {
            self.admin.login_max_failures = 5;
        }
        if self.admin.login_window_secs == 0 {
            self.admin.login_window_secs = 300;
        }
        if let Some(redirect) = &self.admin.redirect_url {
            let parsed = url::Url::parse(redirect).map_err(|_| {
                GatewayError::config("admin.redirect_url must be an absolute http(s) URL")
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(GatewayError::config(
                    "admin.redirect_url must use http or https",
                ));
            }
        }
        if self.scheduler.queue_timeout_secs == 0 {
            self.scheduler.queue_timeout_secs = 30;
        }
        if self.scheduler.provider_max_concurrency == 0 {
            self.scheduler.provider_max_concurrency = 64;
        }
        if self.scheduler.failure_threshold == 0 {
            self.scheduler.failure_threshold = 3;
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.providers {
            let id = p.provider_id();
            if !seen.insert(id.clone()) {
                return Err(GatewayError::config(format!("duplicate provider id {id}")));
            }
            if p.kind != ProviderKind::Mock && p.base_url.is_none() {
                tracing::warn!(provider = %id, "provider has no base_url override; built-in default will be used");
            }
        }
        Ok(())
    }

    /// Gateway API keys from `auth.bootstrap_api_key`, `auth.api_keys` and the
    /// `POLYGATE_API_KEY` override, trimmed and de-duplicated. Empty entries are
    /// rejected during validation.
    pub fn gateway_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        let mut push = |key: &str| {
            let key = key.trim();
            if !key.is_empty() && !keys.iter().any(|existing| existing == key) {
                keys.push(key.to_string());
            }
        };
        if let Some(key) = &self.auth.bootstrap_api_key {
            push(key);
        }
        for key in &self.auth.api_keys {
            push(key);
        }
        keys
    }

    fn validate_gateway_keys(&self) -> Result<(), GatewayError> {
        if let Some(key) = &self.auth.bootstrap_api_key {
            if key.trim().is_empty() {
                return Err(GatewayError::config(
                    "auth.bootstrap_api_key must not be empty",
                ));
            }
        }
        for key in &self.auth.api_keys {
            if key.trim().is_empty() {
                return Err(GatewayError::config("auth.api_keys contains an empty key"));
            }
        }
        for key in self.gateway_keys() {
            if key.chars().count() < MIN_API_KEY_LEN {
                return Err(GatewayError::config(format!(
                    "gateway API key is too short (minimum {MIN_API_KEY_LEN} characters)"
                )));
            }
            if PLACEHOLDER_KEYS
                .iter()
                .any(|placeholder| key.eq_ignore_ascii_case(placeholder))
            {
                return Err(GatewayError::config(
                    "gateway API key is an example placeholder; configure a real key via POLYGATE_API_KEY or auth.api_keys",
                ));
            }
        }
        Ok(())
    }

    /// Ensure the data directory exists.
    pub fn prepare_dirs(&self) -> Result<(), GatewayError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|e| {
            GatewayError::config(format!(
                "cannot create data dir {}: {e}",
                self.data_dir.display()
            ))
        })
    }
}

/// Print a fully commented example used by `polygate config-example`.
pub fn example_config() -> &'static str {
    include_str!("../../config.example.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn load_with(toml: &str, env: &[(&str, &str)]) -> Result<Config, GatewayError> {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml).expect("write config");
        let env: std::collections::HashMap<String, String> = env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        Config::load_with_env(Some(&path), Some(dir.path().to_path_buf()), |key| {
            env.get(key).cloned()
        })
    }

    #[test]
    fn example_config_parses() {
        let cfg: FileConfig = toml::from_str(example_config()).expect("example config must parse");
        assert!(cfg.providers.iter().any(|p| p.kind == ProviderKind::Mock));
        assert_eq!(cfg.server.listen, "127.0.0.1:8787");
    }

    #[test]
    fn admin_defaults_use_real_huawei_endpoints() {
        let admin = AdminConfig::default();
        assert_eq!(
            admin.codearts_sts_url,
            "https://sts.cn-north-4.myhuaweicloud.com"
        );
        assert_eq!(
            admin.codearts_cn_portal_url,
            "https://codearts.huaweicloud.com"
        );
        assert_eq!(
            admin.codearts_intl_portal_url,
            "https://codearts.ap-southeast-1.huaweicloud.com"
        );
    }

    #[test]
    fn environment_overrides_toml() {
        let toml = r#"
[server]
listen = "127.0.0.1:9999"
[auth]
api_keys = ["toml-key-0123456789"]
"#;
        let cfg = load_with(
            toml,
            &[
                ("POLYGATE_LISTEN", "127.0.0.1:7777"),
                ("POLYGATE_API_KEY", "env-key-0123456789abcdef"),
            ],
        )
        .expect("load");

        assert_eq!(cfg.server.listen, "127.0.0.1:7777");
        let keys = cfg.gateway_keys();
        assert!(keys.contains(&"toml-key-0123456789".to_string()));
        assert!(keys.contains(&"env-key-0123456789abcdef".to_string()));
    }

    #[test]
    fn placeholder_and_short_keys_are_rejected() {
        let err = load_with("[auth]\napi_keys = [\"polygate-dev-key\"]\n", &[])
            .expect_err("placeholder key");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("placeholder"), "{err}");

        let err = load_with("[auth]\napi_keys = [\"short-key\"]\n", &[]).expect_err("short key");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("too short"), "{err}");
    }

    #[test]
    fn empty_api_key_entries_are_rejected() {
        let err = load_with("[auth]\napi_keys = [\"\"]\n", &[]).expect_err("empty entry");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("empty key"), "{err}");

        let err =
            load_with("[auth]\nbootstrap_api_key = \"  \"\n", &[]).expect_err("blank bootstrap");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("must not be empty"), "{err}");
    }

    #[test]
    fn master_key_cannot_be_stored_in_config() {
        let err =
            load_with("[auth]\nmaster_key = \"deadbeef\"\n", &[]).expect_err("unknown auth field");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("unknown field"), "{err}");
    }

    #[test]
    fn invalid_listen_and_empty_log_level_are_rejected() {
        let err =
            load_with("[server]\nlisten = \"not-an-address\"\n", &[]).expect_err("invalid listen");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("invalid listen address"), "{err}");

        let err = load_with("[logging]\nlevel = \"  \"\n", &[]).expect_err("empty log level");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("logging.level"), "{err}");
    }

    #[test]
    fn mock_provider_can_be_disabled_by_environment() {
        let cfg = load_with("", &[("POLYGATE_MOCK_ENABLED", "false")]).expect("load");
        assert!(cfg.providers.iter().any(|p| p.kind == ProviderKind::Mock));
        assert!(cfg.providers.iter().all(|p| !p.enabled));

        let cfg = load_with("", &[]).expect("load");
        assert!(cfg
            .providers
            .iter()
            .any(|p| p.kind == ProviderKind::Mock && p.enabled));

        let err = load_with("", &[("POLYGATE_MOCK_ENABLED", "maybe")]).expect_err("bad flag");
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("POLYGATE_MOCK_ENABLED"), "{err}");
    }

    #[test]
    fn process_environment_is_read_by_load() {
        struct EnvGuard(&'static str);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }

        std::env::set_var("POLYGATE_LISTEN", "127.0.0.1:4567");
        let _guard = EnvGuard("POLYGATE_LISTEN");
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cfg = Config::load(None, Some(dir.path().to_path_buf())).expect("load");
        assert_eq!(cfg.server.listen, "127.0.0.1:4567");
    }
}
