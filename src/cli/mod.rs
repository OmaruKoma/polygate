//! Command line interface: `serve`, `doctor` and `config-example`.
//!
//! Configuration precedence (highest first):
//! 1. explicit `--config` / `--data-dir` arguments,
//! 2. `POLYGATE_CONFIG` and the other `POLYGATE_*` environment variables,
//! 3. `<data-dir>/config.toml`,
//! 4. built-in defaults.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::info;

use crate::config::Config;
use crate::error::GatewayError;
use crate::runtime::Gateway;
use crate::security::SecretBox;
use crate::storage::Storage;

#[cfg(test)]
mod tests;

#[derive(Debug, Parser)]
#[command(
    name = "polygate",
    version,
    about = "Multi-provider coding agent gateway",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Data directory for the database and master key.
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// Start the HTTP gateway.
    Serve,
    /// Check configuration, keys, database and listen address.
    Doctor,
    /// Print a complete example configuration.
    ConfigExample,
}

/// Parse arguments and run. Returns the process exit code.
pub async fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return if err.use_stderr() { 2 } else { 0 };
        }
    };
    match execute(cli).await {
        Ok(()) => 0,
        Err(err) => {
            err.log("cli");
            eprintln!("error: {}", err.message);
            1
        }
    }
}

/// Execute one parsed command. Separate from [`run`] so tests can drive every
/// command without spawning a process.
pub async fn execute(cli: Cli) -> Result<(), GatewayError> {
    match cli.command {
        Command::ConfigExample => {
            print!("{}", crate::config::example_config());
            Ok(())
        }
        Command::Doctor => {
            let config = Config::load(cli.config.as_deref(), cli.data_dir)?;
            init_logging(&config.logging.level);
            let report = doctor(&config).await;
            print!("{}", report.render());
            if report.ok() {
                Ok(())
            } else {
                Err(GatewayError::config("doctor reported failing checks"))
            }
        }
        Command::Serve => {
            let config = Config::load(cli.config.as_deref(), cli.data_dir)?;
            init_logging(&config.logging.level);
            info!(version = crate::VERSION, "starting polygate");
            let gateway = Gateway::bind(config).await?;
            println!(
                "polygate {} listening on http://{}",
                crate::VERSION,
                gateway.local_addr()
            );
            gateway.serve(shutdown_signal()).await
        }
    }
}

fn init_logging(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Resolve on Ctrl+C, or SIGTERM on Unix.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("ctrl-c received; shutting down"),
        _ = terminate => info!("SIGTERM received; shutting down"),
    }
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|check| check.ok)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let status = if check.ok { "ok" } else { "FAIL" };
            out.push_str(&format!("[{status}] {}: {}\n", check.name, check.detail));
        }
        out
    }
}

fn pass(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        ok: true,
        detail: detail.into(),
    }
}

fn fail(name: &'static str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        ok: false,
        detail: detail.into(),
    }
}

/// Inspect the resolved configuration without serving. Never prints secret
/// values: key material is only reported as present/absent.
pub async fn doctor(config: &Config) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(pass(
        "config",
        format!(
            "loaded {} provider entries, listen {}",
            config.providers.len(),
            config.server.listen
        ),
    ));

    let keys = config.gateway_keys();
    checks.push(if keys.is_empty() {
        fail(
            "api_key",
            "no gateway API key configured; set POLYGATE_API_KEY or auth.api_keys",
        )
    } else {
        pass("api_key", format!("{} key(s) configured", keys.len()))
    });

    checks.push(master_key_check(config));
    checks.push(database_check(config).await);
    checks.push(providers_check(config));
    checks.push(listen_check(config));

    DoctorReport { checks }
}

fn master_key_check(config: &Config) -> DoctorCheck {
    let from_env = std::env::var("POLYGATE_MASTER_KEY").is_ok();
    let file = config.data_dir.join("master.key");
    if !from_env && !file.exists() {
        return pass(
            "master_key",
            "not configured yet; a new key will be generated on first serve",
        );
    }

    let source = if from_env {
        "environment"
    } else {
        "master.key file"
    };
    match SecretBox::from_env_or_file(&config.data_dir) {
        Ok(_) => pass("master_key", format!("usable ({source}, 32 bytes)")),
        Err(err) => fail("master_key", err.message),
    }
}

async fn database_check(config: &Config) -> DoctorCheck {
    match Storage::open_url(&config.database.url, config.database.max_connections).await {
        Ok(storage) => {
            storage.close().await;
            pass("database", "reachable; migrations applied; pool closed")
        }
        Err(err) => fail("database", err.message),
    }
}

fn providers_check(config: &Config) -> DoctorCheck {
    let enabled: Vec<&crate::config::ProviderConfig> =
        config.providers.iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        return fail("providers", "no enabled providers configured");
    }
    let summary = enabled
        .iter()
        .map(|provider| format!("{}:{}", provider.provider_id(), provider.kind.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    if enabled
        .iter()
        .any(|provider| provider.kind != crate::config::ProviderKind::Mock)
    {
        return fail(
            "providers",
            format!("non-mock provider enabled but not implemented: {summary}"),
        );
    }
    pass("providers", summary)
}

fn listen_check(config: &Config) -> DoctorCheck {
    match config.server.socket_addr() {
        Ok(addr) => match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                drop(listener);
                pass("listen", format!("{addr} is bindable"))
            }
            Err(err) => fail("listen", format!("cannot bind {addr}: {err}")),
        },
        Err(err) => fail("listen", err.message),
    }
}
