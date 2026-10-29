//! Runtime tests: startup failures, registry wiring, real TCP serving,
//! shutdown and persistence.

use std::path::PathBuf;

use serde_json::{json, Value};
use tempfile::TempDir;

use super::Gateway;
use crate::config::{Config, ProviderKind, Region};
use crate::error::ErrorKind;

const KEY: &str = "e2e-key-0123456789abcdef";

fn base_toml(options: &str) -> String {
    format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[logging]
level = "warn"

[[providers]]
kind = "mock"
region = "cn"
enabled = true

[providers.options]
{options}
"#
    )
}

fn write_config(dir: &TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).expect("write config");
    path
}

fn load(dir: &TempDir, body: &str) -> Config {
    let path = write_config(dir, body);
    Config::load_with_env(Some(&path), Some(dir.path().to_path_buf()), |_| None)
        .expect("config must load")
}

async fn bind(dir: &TempDir, body: &str) -> Gateway {
    Gateway::bind(load(dir, body))
        .await
        .expect("gateway must bind")
}

async fn get_json(client: &reqwest::Client, url: String) -> (u16, Value) {
    let response = client.get(url).send().await.expect("request");
    let status = response.status().as_u16();
    let body = response.json().await.expect("json body");
    (status, body)
}

#[tokio::test]
async fn serves_openai_over_a_real_tcp_listener_and_closes_the_pool() {
    let dir = TempDir::new().expect("temp dir");
    let gateway = bind(&dir, &base_toml(r#""mock.reply" = "e2e reply""#)).await;

    assert_eq!(gateway.local_addr().ip().to_string(), "127.0.0.1");
    assert_eq!(gateway.registry().list_enabled().len(), 1);
    assert_eq!(
        gateway.registry().list_enabled()[0].kind,
        crate::config::ProviderKind::Mock
    );

    let storage = gateway.storage();
    assert!(!storage.is_closed());

    let addr = gateway.local_addr();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        gateway
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let (status, body) = get_json(&client, format!("{base}/health")).await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], crate::VERSION);

    let response = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .expect("models without key");
    assert_eq!(response.status().as_u16(), 401);
    let unauthorized = response.text().await.expect("401 body");
    assert!(!unauthorized.contains(KEY), "key leaked in error body");

    let response = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(KEY)
        .send()
        .await
        .expect("models");
    assert_eq!(response.status().as_u16(), 200);
    let models: Value = response.json().await.expect("models json");
    assert!(models["data"]
        .as_array()
        .expect("data")
        .iter()
        .any(|model| model["id"] == "mock-chat"));

    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({
            "model": "mock-chat",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .expect("chat");
    assert_eq!(response.status().as_u16(), 200);
    let chat: Value = response.json().await.expect("chat json");
    assert_eq!(chat["object"], "chat.completion");
    assert_eq!(chat["choices"][0]["message"]["content"], "e2e reply");
    assert_eq!(chat["choices"][0]["finish_reason"], "stop");

    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({
            "model": "mock-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        }))
        .send()
        .await
        .expect("stream");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let body = response.text().await.expect("sse body");
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    let content: String = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .filter_map(|chunk| {
            chunk["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(content, "e2e reply");
    assert!(body.contains("\"usage\""));
    assert!(body.contains("data: [DONE]"));

    shutdown_tx.send(()).expect("signal shutdown");
    server
        .await
        .expect("server task")
        .expect("graceful shutdown");
    assert!(storage.is_closed(), "pool must be closed after shutdown");
}

#[tokio::test]
async fn restart_keeps_database_state_and_serves_tool_calls() {
    let dir = TempDir::new().expect("temp dir");
    let options = r#""mock.reply" = ''
"mock.tool_calls" = '[{"id":"call-1","name":"read_file","arguments":{"path":"a.txt"}}]'
"#;
    let body = base_toml(options);

    let gateway = bind(&dir, &body).await;
    let storage = gateway.storage();
    storage
        .settings()
        .set("e2e", "persisted")
        .await
        .expect("write setting");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        gateway
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    shutdown_tx.send(()).expect("signal shutdown");
    server.await.expect("server task").expect("shutdown");
    assert!(storage.is_closed());

    // Restart on the same data directory.
    let gateway = bind(&dir, &body).await;
    let storage = gateway.storage();
    assert_eq!(
        storage
            .settings()
            .get("e2e")
            .await
            .expect("read setting")
            .as_deref(),
        Some("persisted")
    );

    let addr = gateway.local_addr();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        gateway
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({
            "model": "mock-chat",
            "messages": [{"role": "user", "content": "read it"}],
            "tools": [{"type": "function", "function": {"name": "read_file", "parameters": {"type": "object"}}}]
        }))
        .send()
        .await
        .expect("tool call chat");
    assert_eq!(response.status().as_u16(), 200);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read_file"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .expect("arguments")
        )
        .expect("json"),
        json!({ "path": "a.txt" })
    );

    shutdown_tx.send(()).expect("signal shutdown");
    server.await.expect("server task").expect("shutdown");
    assert!(storage.is_closed());
}

#[tokio::test]
async fn bind_fails_without_a_gateway_key() {
    let dir = TempDir::new().expect("temp dir");
    let body = r#"
[server]
listen = "127.0.0.1:0"

[[providers]]
kind = "mock"
region = "cn"
enabled = true
"#;
    let err = Gateway::bind(load(&dir, body))
        .await
        .expect_err("startup must fail");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("no gateway API key"), "{err}");
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            std::env::set_var(self.key, value);
        }
    }
}

#[tokio::test]
async fn bind_fails_on_an_unusable_master_key() {
    let _guard = EnvGuard::remove("POLYGATE_MASTER_KEY");
    let dir = TempDir::new().expect("temp dir");
    std::fs::write(dir.path().join("master.key"), "this-is-not-a-master-key")
        .expect("write key file");

    let err = Gateway::bind(load(&dir, &base_toml("")))
        .await
        .expect_err("startup must fail");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("master key"), "{err}");
    assert!(
        !err.message.contains("this-is-not-a-master-key"),
        "key material must not leak: {err}"
    );
}

#[tokio::test]
async fn removed_codebuddy_kind_is_rejected_by_config() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "codebuddy"
region = "cn"
enabled = true
"#
    );
    let path = write_config(&dir, &body);
    let err = Config::load_with_env(Some(&path), Some(dir.path().to_path_buf()), |_| None)
        .expect_err("codebuddy is no longer a provider kind");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("codebuddy") || err.message.contains("unknown variant"),
        "{err}"
    );
}

#[tokio::test]
async fn bind_accepts_the_trae_provider() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "trae"
region = "cn"
enabled = true
verification = "experimental"
"#
    );
    let gateway = Gateway::bind(load(&dir, &body))
        .await
        .expect("trae must be registrable");
    let providers = gateway.registry().list_enabled();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].kind, ProviderKind::Trae);
    assert_eq!(providers[0].region, Region::Cn);
}

#[tokio::test]
async fn bind_accepts_the_qoder_provider() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "qoder"
region = "cn"
enabled = true
verification = "experimental"
"#
    );
    let gateway = Gateway::bind(load(&dir, &body))
        .await
        .expect("qoder must be registrable");
    let providers = gateway.registry().list_enabled();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].kind, ProviderKind::Qoder);
    assert_eq!(providers[0].region, Region::Cn);
}

#[tokio::test]
async fn bind_accepts_the_workbuddy_provider() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "workbuddy"
region = "intl"
enabled = true
verification = "experimental"
"#
    );
    let gateway = Gateway::bind(load(&dir, &body))
        .await
        .expect("workbuddy must be registrable");
    let providers = gateway.registry().list_enabled();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].kind, ProviderKind::Workbuddy);
    assert_eq!(providers[0].region, Region::Intl);
}

#[tokio::test]
async fn bind_fails_when_the_address_is_already_in_use() {
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("blocker");
    let addr = blocker.local_addr().expect("blocker addr");
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "{addr}"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "mock"
region = "cn"
enabled = true
"#
    );
    let err = Gateway::bind(load(&dir, &body))
        .await
        .expect_err("startup must fail");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("cannot bind"), "{err}");
}

#[tokio::test]
async fn registry_contains_every_enabled_provider() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "mock"
region = "cn"
enabled = true

[[providers]]
kind = "mock"
region = "intl"
enabled = false
"#
    );
    let gateway = bind(&dir, &body).await;
    let enabled = gateway.registry().list_enabled();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, "mock-cn");
    assert_eq!(
        gateway.registry().len(),
        1,
        "disabled providers are not registered"
    );
}

// ---------------------------------------------------------------------------
// M13-B: CodeArts registration, discovery and sticky sessions end to end
// ---------------------------------------------------------------------------

const CODEARTS_MODELS: &str = include_str!("../providers/codearts/fixtures/models.json");
const CODEARTS_SSE: &str = include_str!("../providers/codearts/fixtures/chat_sse.txt");

#[tokio::test]
async fn codearts_provider_is_registered_and_session_binding_is_sticky() {
    use std::sync::Arc;

    use crate::account_service::{AccountService, CreateAccount};
    use crate::config::{ProviderKind, Region};
    use crate::security::SecretBox;
    use crate::storage::Storage;
    use crate::test_support::{spawn_server, Reply};

    let upstream = spawn_server(
        |request: &crate::test_support::Recorded| match request.path() {
            "/v1/model/builtin" => Reply::json(200, CODEARTS_MODELS),
            "/api/v2/chat/completions" => Reply::sse(CODEARTS_SSE),
            _ => Reply::json(404, "{}"),
        },
    )
    .await;

    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[logging]
level = "warn"

[[providers]]
kind = "codearts"
region = "cn"
enabled = true
base_url = "{url}"

[providers.options]
sts_url = "{url}"
"#,
        url = upstream.base_url
    );
    let config = load(&dir, &body);
    let database_url = config.database.url.clone();

    // Accounts are imported before startup in this milestone (there is no admin
    // HTTP API yet), so discovery can see a real credential.
    let secret = Arc::new(SecretBox::from_env_or_file(&config.data_dir).expect("master key"));
    let storage = Storage::open_url(&database_url, 2).await.expect("db");
    let accounts = AccountService::new(storage.clone(), secret);
    accounts
        .create(CreateAccount::new(
            ProviderKind::Codearts,
            Region::Cn,
            "alpha",
            Some(json!({
                "access_key_id": "AKID",
                "secret_access_key": "SECRET-KEY",
                "security_token": "STS-TOKEN",
                "domain_id": "domain-1",
            })),
        ))
        .await
        .expect("account");
    storage.close().await;

    let gateway = Gateway::bind(config).await.expect("gateway binds");
    let enabled = gateway.registry().list_enabled();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].kind, ProviderKind::Codearts);
    assert_eq!(
        enabled[0].verification,
        crate::providers::VerificationState::Experimental
    );

    let addr = gateway.local_addr();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        gateway
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let models: Value = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(KEY)
        .send()
        .await
        .expect("models")
        .json()
        .await
        .expect("models json");
    assert!(models["data"]
        .as_array()
        .expect("data")
        .iter()
        .any(|model| model["id"] == "glm-5.2"));

    let chat = json!({
        "model": "glm-5.2",
        "messages": [{"role": "user", "content": "hello"}],
    });
    for session in ["session-1", "session-1"] {
        let response = client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(KEY)
            .header("x-session-id", session)
            .json(&chat)
            .send()
            .await
            .expect("chat");
        assert_eq!(response.status().as_u16(), 200);
        let body: Value = response.json().await.expect("chat json");
        assert_eq!(body["choices"][0]["message"]["content"], "Hello");
    }

    let streaming = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(KEY)
        .header("x-session-id", "session-2")
        .json(&json!({
            "model": "glm-5.2",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true,
            "stream_options": {"include_usage": true},
        }))
        .send()
        .await
        .expect("stream");
    assert_eq!(streaming.status().as_u16(), 200);
    let body = streaming.text().await.expect("sse body");
    assert!(body.contains("Hello"), "SSE body: {body}");
    assert!(body.contains("[DONE]"));
    assert!(body.contains("\"usage\""));

    let chat_requests: Vec<_> = upstream
        .requests()
        .into_iter()
        .filter(|request| request.path() == "/api/v2/chat/completions")
        .collect();
    assert_eq!(chat_requests.len(), 3);
    let ids: Vec<String> = chat_requests
        .iter()
        .map(|request| {
            let body: Value = serde_json::from_str(&request.body).expect("body");
            body["chat_id"].as_str().expect("chat_id").to_string()
        })
        .collect();
    assert_eq!(ids[0], ids[1]);
    assert_ne!(ids[0], ids[2]);
    assert!(ids
        .iter()
        .all(|id| id.len() == 32 && id.chars().all(|ch| ch.is_ascii_hexdigit())));
    assert!(chat_requests[0]
        .header("Authorization")
        .expect("authorization")
        .starts_with("SDK-HMAC-SHA256 Access=AKID"));

    let _ = shutdown_tx.send(());
    server.await.expect("server task").expect("served");
}

#[tokio::test]
async fn codearts_provider_without_an_account_advertises_no_models() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[logging]
level = "warn"

[[providers]]
kind = "codearts"
region = "cn"
enabled = true
base_url = "http://127.0.0.1:1"

[providers.options]
sts_url = "http://127.0.0.1:1"
"#
    );
    let gateway = bind(&dir, &body).await;
    assert_eq!(gateway.registry().list_enabled().len(), 1);
    // Discovery without a credential must not invent a catalogue.
    let addr = gateway.local_addr();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        gateway
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .bearer_auth(KEY)
        .send()
        .await
        .expect("models");
    assert_eq!(response.status().as_u16(), 200);
    let models: Value = response.json().await.expect("json");
    assert!(models["data"].as_array().expect("data").is_empty());
    let _ = shutdown_tx.send(());
    server.await.expect("server task").expect("served");
}

#[tokio::test]
async fn client_identity_option_accepts_both_spellings() {
    for value in ["impersonate", "honest"] {
        let dir = TempDir::new().expect("temp dir");
        let body = format!(
            r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "trae"
region = "cn"
enabled = true

[providers.options]
"client.identity" = "{value}"
"#
        );
        Gateway::bind(load(&dir, &body))
            .await
            .unwrap_or_else(|error| panic!("`{value}` must be accepted: {error}"));
    }
}

#[tokio::test]
async fn an_unknown_client_identity_value_is_a_config_error() {
    let dir = TempDir::new().expect("temp dir");
    let body = format!(
        r#"
[server]
listen = "127.0.0.1:0"

[auth]
api_keys = ["{KEY}"]

[[providers]]
kind = "trae"
region = "cn"
enabled = true

[providers.options]
"client.identity" = "spoof"
"#
    );
    let error = Gateway::bind(load(&dir, &body))
        .await
        .expect_err("unknown identity must fail");
    assert!(error.to_string().contains("client.identity"), "{error}");
}
