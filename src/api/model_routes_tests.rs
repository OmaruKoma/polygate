//! Alias + multi-target routing over the real Axum router.
//!
//! Two mock provider instances expose the same native model id. Nothing here
//! touches a network or a real credential.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{router, ApiKeyAuth, ApiState};
use crate::config::Region;
use crate::model_service::{create_target, ModelRouteService};
use crate::providers::{MockConfig, MockProvider, MockReply, ProviderRegistry, UpstreamModel};
use crate::storage::Storage;

const KEY: &str = "routes-test-key";

struct RoutesApp {
    app: Router,
    registry: Arc<ProviderRegistry>,
    catalog: crate::api::ModelCatalog,
    storage: Storage,
}

async fn routes_app() -> RoutesApp {
    let cn = Arc::new(MockProvider::new(MockConfig {
        id: "mock-cn".into(),
        models: vec![UpstreamModel::new("shared")],
        reply: MockReply::fixed("from-cn"),
        ..MockConfig::default()
    }));
    let intl = Arc::new(MockProvider::new(MockConfig {
        id: "mock-intl".into(),
        region: Region::Intl,
        models: vec![UpstreamModel::new("shared")],
        reply: MockReply::fixed("from-intl"),
        ..MockConfig::default()
    }));
    let registry = Arc::new(ProviderRegistry::new());
    registry.register(cn).expect("register cn");
    registry.register(intl).expect("register intl");
    let catalog = super::ModelCatalog::discover(
        &registry,
        &crate::accounts::Credential::from_value(json!({})),
    )
    .await;
    let storage = Storage::open_memory().await.expect("storage");
    let service = Arc::new(ModelRouteService::new(storage.clone()));
    let state = ApiState::new(
        Arc::clone(&registry),
        catalog.clone(),
        ApiKeyAuth::new(vec![KEY.to_string()]).expect("key"),
    )
    .with_model_routes(service, storage.clone());
    RoutesApp {
        app: router(state),
        registry,
        catalog,
        storage,
    }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    (status, body.to_vec())
}

fn chat(model: &str) -> Request<Body> {
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
    });
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
async fn duplicate_native_ids_are_ambiguous_until_disambiguated() {
    let harness = routes_app().await;
    let (status, body) = send(&harness.app, chat("shared")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&body).expect("json");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("several providers"), "{message}");
    assert!(message.contains("mock-cn") && message.contains("mock-intl"));

    let (status, body) = send(&harness.app, chat("mock-intl::shared")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(body["model"], "shared");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        Value::String("from-intl".into())
    );
}

async fn add_target(
    harness: &RoutesApp,
    alias: &str,
    provider: &str,
    upstream: &str,
    priority: i64,
    capabilities: Value,
) {
    create_target(
        &harness.storage,
        &harness.registry,
        &harness.catalog,
        crate::model_service::NewTarget {
            alias: alias.to_string(),
            provider: provider.to_string(),
            upstream_model: upstream.to_string(),
            display_name: None,
            priority,
            weight: 1,
            capabilities,
        },
    )
    .await
    .expect("create target");
}

#[tokio::test]
async fn alias_routes_by_priority_and_echoes_the_alias() {
    let harness = routes_app().await;
    add_target(
        &harness,
        "coding-auto",
        "mock-intl",
        "shared",
        0,
        Value::Null,
    )
    .await;
    add_target(
        &harness,
        "coding-auto",
        "mock-cn",
        "shared",
        10,
        Value::Null,
    )
    .await;

    let (status, body) = send(&harness.app, chat("coding-auto")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(body["model"], "coding-auto");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        Value::String("from-cn".into())
    );
}

#[tokio::test]
async fn a_failing_target_falls_through_to_the_next_one() {
    let harness = routes_app().await;
    // The first target names a model the provider does not serve; that is a
    // safe pre-content failure, so the next target must be tried.
    add_target(
        &harness,
        "fallback",
        "mock-cn",
        "missing-model",
        10,
        Value::Null,
    )
    .await;
    add_target(&harness, "fallback", "mock-intl", "shared", 0, Value::Null).await;
    let (status, body) = send(&harness.app, chat("fallback")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let body: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        Value::String("from-intl".into())
    );
}

#[tokio::test]
async fn capability_gaps_refuse_instead_of_routing_tools_to_a_models_that_lacks_them() {
    let harness = routes_app().await;
    add_target(
        &harness,
        "no-tools",
        "mock-cn",
        "shared",
        0,
        json!({"tools": false}),
    )
    .await;
    let body = json!({
        "model": "no-tools",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{
            "type": "function",
            "function": {"name": "f", "parameters": {"type": "object"}}
        }]
    });
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let (status, body) = send(&harness.app, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&body).expect("json");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("tool calling"), "{message}");
}

#[tokio::test]
async fn an_alias_cannot_shadow_a_native_model_id() {
    let harness = routes_app().await;
    let error = create_target(
        &harness.storage,
        &harness.registry,
        &harness.catalog,
        crate::model_service::NewTarget {
            alias: "shared".into(),
            provider: "mock-cn".into(),
            upstream_model: "shared".into(),
            display_name: None,
            priority: 0,
            weight: 1,
            capabilities: Value::Null,
        },
    )
    .await
    .expect_err("alias must not shadow a native id");
    assert!(error.message.contains("conflicts with a native model id"));
}

#[tokio::test]
async fn model_listing_includes_usable_aliases() {
    let harness = routes_app().await;
    add_target(&harness, "coding-auto", "mock-cn", "shared", 0, Value::Null).await;
    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = send(&harness.app, request).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).expect("json");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(ids.contains(&"coding-auto"));
    assert!(ids.contains(&"shared"));
}
