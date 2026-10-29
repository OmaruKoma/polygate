use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::accounts::Credential;
use crate::config::{ProviderKind, Region, Verification};
use crate::error::ErrorKind;
use crate::protocol::ChatRequest;

/// Minimal provider for registry tests. Only `info`/`capabilities` are
/// implemented; every operation falls back to the trait defaults, which is
/// exactly what the tests for unsupported capabilities need.
struct MockProvider {
    info: ProviderInfo,
    capabilities: Capabilities,
}

impl MockProvider {
    fn new(id: &str, kind: ProviderKind, region: Region) -> Self {
        Self {
            info: ProviderInfo {
                id: id.into(),
                kind,
                region,
                display_name: format!("{id} test instance"),
                version: "0.1.0-test".into(),
                verification: VerificationState::Experimental,
                enabled: true,
            },
            capabilities: Capabilities::default(),
        }
    }

    fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
}

fn mock(id: &str, kind: ProviderKind, region: Region) -> Arc<MockProvider> {
    Arc::new(MockProvider::new(id, kind, region))
}

fn key(kind: ProviderKind, region: Region) -> ProviderKey {
    ProviderKey::new(kind, region)
}

fn credential() -> Credential {
    Credential::from_value(serde_json::json!({ "token": "test-only" }))
}

#[test]
fn registers_two_mock_providers() {
    let registry = ProviderRegistry::new();
    assert!(registry.is_empty());

    let cn = registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Cn))
        .expect("register cn");
    let intl = registry
        .register(mock("mock-intl", ProviderKind::Mock, Region::Intl))
        .expect("register intl");

    assert!(cn.enabled);
    assert_eq!(cn.verification, VerificationState::Experimental);
    assert_eq!(intl.id, "mock-intl");
    assert_eq!(registry.len(), 2);
    let ids: Vec<String> = registry.list_enabled().into_iter().map(|i| i.id).collect();
    assert_eq!(ids, vec!["mock-cn".to_string(), "mock-intl".to_string()]);
}

#[test]
fn lookup_by_kind_and_region() {
    let registry = ProviderRegistry::new();
    let provider: ProviderRef = mock("mock-cn", ProviderKind::Mock, Region::Cn);
    registry.register(provider.clone()).expect("register");

    let found = registry
        .get(&key(ProviderKind::Mock, Region::Cn))
        .expect("registered");
    assert!(Arc::ptr_eq(&found, &provider));

    let required = registry
        .require(&key(ProviderKind::Mock, Region::Cn))
        .expect("enabled");
    assert!(Arc::ptr_eq(&required, &provider));

    assert!(registry
        .get(&key(ProviderKind::Mock, Region::Intl))
        .is_none());
    assert!(registry
        .require(&key(ProviderKind::Mock, Region::Intl))
        .is_err());
    assert_eq!(registry.keys(), vec![key(ProviderKind::Mock, Region::Cn)]);
}

#[test]
fn duplicate_registration_is_rejected() {
    let registry = ProviderRegistry::new();
    registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Cn))
        .expect("first");

    let same_key = registry
        .register(mock("other-cn", ProviderKind::Mock, Region::Cn))
        .expect_err("duplicate key");
    assert_eq!(same_key.kind, ErrorKind::Config);
    assert!(
        same_key.message.contains("already registered"),
        "{same_key}"
    );

    let same_id = registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Intl))
        .expect_err("duplicate id");
    assert_eq!(same_id.kind, ErrorKind::Config);
    assert!(
        same_id.message.contains("already used by mock-cn"),
        "{same_id}"
    );

    assert_eq!(registry.len(), 1);
}

#[test]
fn registration_requires_a_non_empty_id() {
    let registry = ProviderRegistry::new();
    let err = registry
        .register(mock(" ", ProviderKind::Mock, Region::Cn))
        .expect_err("empty id");
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(registry.is_empty());
}

#[test]
fn disabling_removes_a_provider_from_request_routing() {
    let registry = ProviderRegistry::new();
    registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Cn))
        .expect("register");

    registry
        .set_enabled(&key(ProviderKind::Mock, Region::Cn), false)
        .expect("disable");
    let err = registry
        .require(&key(ProviderKind::Mock, Region::Cn))
        .expect_err("disabled");
    assert_eq!(err.kind, ErrorKind::NoCapacity);
    assert!(err.message.contains("is disabled"), "{err}");
    assert!(!registry.is_enabled(&key(ProviderKind::Mock, Region::Cn)));
    assert!(registry.list_enabled().is_empty());
    // Inspection still works, so the admin UI can re-enable it.
    assert!(registry.get(&key(ProviderKind::Mock, Region::Cn)).is_some());
    assert_eq!(registry.list_all().len(), 1);

    registry
        .set_enabled(&key(ProviderKind::Mock, Region::Cn), true)
        .expect("enable");
    assert!(registry
        .require(&key(ProviderKind::Mock, Region::Cn))
        .is_ok());

    let unknown = registry
        .set_enabled(&key(ProviderKind::Mock, Region::Intl), true)
        .expect_err("unknown provider");
    assert_eq!(unknown.kind, ErrorKind::ModelUnavailable);
}

#[test]
fn capabilities_default_to_false() {
    let caps = Capabilities::default();
    for capability in Capability::ALL {
        assert!(!caps.get(*capability), "{capability} must default to false");
    }
    assert!(caps.declared().is_empty());
    assert_eq!(caps.class(), None);
    assert_eq!(
        serde_json::to_value(caps).expect("serialize"),
        serde_json::json!({
            "authentication": false,
            "model_discovery": false,
            "quota": false,
            "chat": false,
            "streaming": false,
            "tools": false,
            "native_session": false,
            "agent_task": false,
            "cancellation": false,
        })
    );

    let registry = ProviderRegistry::new();
    registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Cn))
        .expect("register");
    let stored = registry
        .capabilities(&key(ProviderKind::Mock, Region::Cn))
        .expect("registered");
    assert_eq!(stored, Capabilities::default());
}

#[test]
fn capability_bits_are_wired_to_names() {
    let mut caps = Capabilities::default();
    caps.set(Capability::Chat, true);
    assert!(caps.get(Capability::Chat));
    caps.set(Capability::Chat, false);
    assert!(!caps.get(Capability::Chat));

    let model = Capabilities {
        chat: true,
        streaming: true,
        tools: true,
        ..Default::default()
    };
    let agent = Capabilities {
        agent_task: true,
        ..Default::default()
    };
    let hybrid = Capabilities {
        chat: true,
        agent_task: true,
        ..Default::default()
    };

    assert_eq!(model.class(), Some(ProviderClass::Model));
    assert_eq!(agent.class(), Some(ProviderClass::Agent));
    assert_eq!(hybrid.class(), Some(ProviderClass::Hybrid));
    assert_eq!(hybrid.declared(), vec!["chat", "agent_task"]);
}

#[tokio::test]
async fn default_operations_return_unsupported() {
    let provider = MockProvider::new("mock-cn", ProviderKind::Mock, Region::Cn);
    let credential = credential();

    let checks: Vec<(&str, ErrorKind)> = vec![
        (
            "authentication",
            provider.auth_status(&credential).await.unwrap_err().kind,
        ),
        (
            "model_discovery",
            provider.list_models(&credential).await.unwrap_err().kind,
        ),
        ("quota", provider.quota(&credential).await.unwrap_err().kind),
        (
            "chat",
            provider
                .chat(&credential, ChatRequest::new("mock", Vec::new()))
                .await
                .unwrap_err()
                .kind,
        ),
        (
            "streaming",
            match provider
                .chat_stream(&credential, ChatRequest::new("mock", Vec::new()))
                .await
            {
                Ok(_) => panic!("streaming must default to Unsupported"),
                Err(err) => err.kind,
            },
        ),
        (
            "agent_task",
            provider
                .agent_task(AgentTaskRequest::new("task-1", "mock", "do it"))
                .await
                .unwrap_err()
                .kind,
        ),
        (
            "cancellation",
            provider
                .cancel(CancelRequest::new("request-1"))
                .await
                .unwrap_err()
                .kind,
        ),
    ];

    for (capability, kind) in checks {
        assert_eq!(kind, ErrorKind::Unsupported, "{capability}");
    }

    let err = provider
        .agent_task(AgentTaskRequest::new("task-1", "mock", "do it"))
        .await
        .unwrap_err();
    assert!(err.message.contains("mock-cn"), "{err}");
    assert!(err.message.contains("agent_task"), "{err}");
    assert_eq!(err.kind.http_status().as_u16(), 501);
}

#[test]
fn cn_and_international_are_independent_providers() {
    let registry = ProviderRegistry::new();
    registry
        .register(mock("qoder-cn", ProviderKind::Qoder, Region::Cn))
        .expect("register cn");
    registry
        .register(mock("qoder-intl", ProviderKind::Qoder, Region::Intl))
        .expect("register intl");

    let cn_key = key(ProviderKind::Qoder, Region::Cn);
    let intl_key = key(ProviderKind::Qoder, Region::Intl);
    assert_ne!(cn_key, intl_key);
    assert_eq!(cn_key.to_string(), "qoder-cn");
    assert_eq!(intl_key.to_string(), "qoder-intl");

    let cn = registry.get(&cn_key).expect("cn");
    let intl = registry.get(&intl_key).expect("intl");
    assert_ne!(cn.info().id, intl.info().id);
    assert_eq!(cn.info().region, Region::Cn);
    assert_eq!(intl.info().region, Region::Intl);

    // Disabling one region leaves the other one routable.
    registry.set_enabled(&cn_key, false).expect("disable cn");
    assert!(registry.require(&cn_key).is_err());
    assert!(registry.require(&intl_key).is_ok());
    assert_eq!(registry.list_enabled().len(), 1);
}

#[test]
fn concurrent_readers_see_a_consistent_registry() {
    let registry = Arc::new(ProviderRegistry::new());
    let cn_key = key(ProviderKind::Mock, Region::Cn);
    let intl_key = key(ProviderKind::Mock, Region::Intl);
    registry
        .register(mock("mock-cn", ProviderKind::Mock, Region::Cn))
        .expect("register cn");
    registry
        .register(mock("mock-intl", ProviderKind::Mock, Region::Intl))
        .expect("register intl");

    std::thread::scope(|scope| {
        let writer = {
            let registry = Arc::clone(&registry);
            scope.spawn(move || {
                for i in 0..500 {
                    registry.set_enabled(&cn_key, i % 2 == 0).expect("toggle");
                }
                registry.set_enabled(&cn_key, true).expect("restore");
            })
        };

        for _ in 0..8 {
            let registry = Arc::clone(&registry);
            scope.spawn(move || {
                for _ in 0..500 {
                    assert_eq!(registry.len(), 2);
                    assert!(registry.get(&cn_key).is_some());
                    assert!(registry.get(&intl_key).is_some());
                    assert!(registry.require(&intl_key).is_ok());
                    assert!(registry.list_enabled().len() <= 2);
                    assert_eq!(registry.list_all().len(), 2);
                    assert!(registry.capabilities(&intl_key).is_some());
                }
            });
        }

        writer.join().expect("writer");
    });

    assert!(registry.is_enabled(&cn_key));
    assert_eq!(registry.list_enabled().len(), 2);
}

#[test]
fn verification_state_serialises_and_maps_from_config() {
    let table = [
        (VerificationState::Verified, "verified"),
        (VerificationState::Experimental, "experimental"),
        (VerificationState::Unsupported, "unsupported"),
        (VerificationState::Unavailable, "unavailable"),
    ];
    for (state, name) in table {
        assert_eq!(
            serde_json::to_value(state).expect("serialize"),
            serde_json::Value::String(name.into())
        );
    }

    assert!(VerificationState::Verified.is_verified());
    assert!(!VerificationState::Experimental.is_verified());
    assert!(VerificationState::Verified.routable());
    assert!(VerificationState::Experimental.routable());
    assert!(!VerificationState::Unsupported.routable());
    assert!(!VerificationState::Unavailable.routable());

    assert_eq!(
        VerificationState::from(Verification::Mock),
        VerificationState::Experimental
    );
    assert_eq!(
        VerificationState::from(Verification::Verified),
        VerificationState::Verified
    );
    assert_eq!(
        VerificationState::from(Verification::Unavailable),
        VerificationState::Unavailable
    );

    let info = mock("qoder-intl", ProviderKind::Qoder, Region::Intl).info();
    let json = serde_json::to_value(&info).expect("serialize info");
    assert_eq!(json["id"], "qoder-intl");
    assert_eq!(json["kind"], "qoder");
    assert_eq!(json["region"], "intl");
    assert_eq!(json["verification"], "experimental");
    assert_eq!(json["enabled"].as_bool(), Some(true));
    assert_eq!(json["version"], "0.1.0-test");
    assert!(!info.is_mock());

    let restored: ProviderInfo = serde_json::from_value(json).expect("deserialize info");
    assert_eq!(restored.key(), key(ProviderKind::Qoder, Region::Intl));
    assert!(mock("mock-cn", ProviderKind::Mock, Region::Cn)
        .info()
        .is_mock());
}

#[test]
fn unsupported_capability_lookup_returns_a_classified_error() {
    let registry = ProviderRegistry::new();
    let chat_only = Arc::new(
        MockProvider::new("mock-cn", ProviderKind::Mock, Region::Cn).with_capabilities(
            Capabilities {
                chat: true,
                streaming: true,
                ..Default::default()
            },
        ),
    );
    registry.register(chat_only).expect("register");

    let cn_key = key(ProviderKind::Mock, Region::Cn);
    assert!(registry
        .require_capability(&cn_key, Capability::Chat)
        .is_ok());
    assert!(registry
        .require_capability(&cn_key, Capability::Streaming)
        .is_ok());

    let err = registry
        .require_capability(&cn_key, Capability::AgentTask)
        .expect_err("agent task not declared");
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("agent_task"), "{err}");

    let unknown = registry
        .require_capability(&key(ProviderKind::Trae, Region::Cn), Capability::Chat)
        .expect_err("unregistered");
    assert_eq!(unknown.kind, ErrorKind::ModelUnavailable);

    registry.set_enabled(&cn_key, false).expect("disable");
    let disabled = registry
        .require_capability(&cn_key, Capability::Chat)
        .expect_err("disabled");
    assert_eq!(disabled.kind, ErrorKind::NoCapacity);

    assert!(registry
        .verification(&key(ProviderKind::Trae, Region::Cn))
        .is_none());
    assert_eq!(
        registry.verification(&cn_key),
        Some(VerificationState::Experimental)
    );
}
