//! CLI, configuration precedence and doctor tests.

use std::path::PathBuf;

use clap::Parser;
use tempfile::TempDir;

use super::*;
use crate::config::{FileConfig, ProviderKind};
use crate::error::ErrorKind;

const KEY: &str = "doctor-key-0123456789abcdef";

fn valid_body(listen: &str) -> String {
    format!(
        r#"
[server]
listen = "{listen}"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "mock"
region = "cn"
enabled = true
"#
    )
}

fn load(dir: &TempDir, body: &str) -> Config {
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).expect("write config");
    Config::load_with_env(Some(&path), Some(dir.path().to_path_buf()), |_| None)
        .expect("config must load")
}

fn check<'a>(report: &'a DoctorReport, name: &str) -> &'a DoctorCheck {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("missing check {name}"))
}

#[test]
fn help_lists_every_command() {
    let err = Cli::try_parse_from(["polygate", "--help"]).expect_err("help");
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    let text = err.to_string();
    for command in ["serve", "doctor", "config-example"] {
        assert!(text.contains(command), "{command} missing from help");
    }
}

#[test]
fn parses_subcommands_and_global_options() {
    let cli = Cli::try_parse_from([
        "polygate",
        "--config",
        "custom.toml",
        "--data-dir",
        "state",
        "serve",
    ])
    .expect("parse serve");
    assert_eq!(cli.command, Command::Serve);
    assert_eq!(cli.config, Some(PathBuf::from("custom.toml")));
    assert_eq!(cli.data_dir, Some(PathBuf::from("state")));

    let cli = Cli::try_parse_from(["polygate", "doctor"]).expect("parse doctor");
    assert_eq!(cli.command, Command::Doctor);

    let cli = Cli::try_parse_from(["polygate", "config-example"]).expect("parse example");
    assert_eq!(cli.command, Command::ConfigExample);
}

#[tokio::test]
async fn config_example_is_complete_and_contains_no_key() {
    let text = crate::config::example_config();
    let parsed: FileConfig = toml::from_str(text).expect("example must parse");
    assert!(parsed.auth.api_keys.is_empty());
    assert!(parsed.auth.bootstrap_api_key.is_none());
    assert!(parsed
        .providers
        .iter()
        .any(|provider| provider.kind == ProviderKind::Mock && provider.enabled));
    assert!(!text.contains("pg-live"));

    let cli = Cli::try_parse_from(["polygate", "config-example"]).expect("parse");
    execute(cli).await.expect("config-example command");
}

#[tokio::test]
async fn doctor_passes_and_never_prints_the_key() {
    let dir = TempDir::new().expect("temp dir");
    let config = load(&dir, &valid_body("127.0.0.1:0"));
    let report = doctor(&config).await;

    assert!(report.ok(), "{}", report.render());
    let names: Vec<&str> = report.checks.iter().map(|check| check.name).collect();
    assert_eq!(
        names,
        vec![
            "config",
            "api_key",
            "master_key",
            "database",
            "providers",
            "listen"
        ]
    );
    assert!(report.render().contains("1 key(s) configured"));
    assert!(!report.render().contains(KEY));
}

#[tokio::test]
async fn doctor_reports_a_missing_api_key() {
    let dir = TempDir::new().expect("temp dir");
    let body = r#"
[server]
listen = "127.0.0.1:0"

[[providers]]
kind = "mock"
region = "cn"
enabled = true
"#;
    let config = load(&dir, body);
    let report = doctor(&config).await;

    assert!(!report.ok());
    let api_key = check(&report, "api_key");
    assert!(!api_key.ok);
    assert!(api_key.detail.contains("POLYGATE_API_KEY"), "{api_key:?}");
    assert!(check(&report, "database").ok);
}

#[tokio::test]
async fn doctor_reports_an_unusable_master_key() {
    let previous = std::env::var_os("POLYGATE_MASTER_KEY");
    std::env::remove_var("POLYGATE_MASTER_KEY");
    let dir = TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("master.key"), "garbage-master-key").expect("write key");
    let config = load(&dir, &valid_body("127.0.0.1:0"));

    let report = doctor(&config).await;
    let master = check(&report, "master_key");
    assert!(!master.ok);
    assert!(!report.render().contains("garbage-master-key"));

    if let Some(value) = previous {
        std::env::set_var("POLYGATE_MASTER_KEY", value);
    }
}

#[tokio::test]
async fn doctor_reports_a_busy_listen_address() {
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("blocker");
    let addr = blocker.local_addr().expect("addr");
    let dir = TempDir::new().expect("temp dir");
    let config = load(&dir, &valid_body(&addr.to_string()));

    let report = doctor(&config).await;
    let listen = check(&report, "listen");
    assert!(!listen.ok);
    assert!(listen.detail.contains("cannot bind"), "{listen:?}");
}

#[tokio::test]
async fn serve_command_rejects_invalid_configuration_before_starting() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[server]\nlisten = \"not-an-address\"\n[auth]\napi_keys = [\"doctor-key-0123456789abcdef\"]\n",
    )
    .expect("write config");

    let cli = Cli {
        config: Some(path),
        data_dir: Some(dir.path().to_path_buf()),
        command: Command::Serve,
    };
    let err = execute(cli).await.expect_err("invalid listen must fail");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("invalid listen address"), "{err}");
}
