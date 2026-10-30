//! HTTP surface: OpenAI-compatible Chat Completions, model listing and health.
//!
//! Request path: API key → body parsing/validation → model catalogue →
//! provider registry → provider → internal `ChatResponse` → OpenAI JSON/SSE.
//! The API layer never talks to a concrete provider: it resolves handles from
//! the registry and checks declared capabilities before calling.
//!
//! M8 only wires the mock provider. There is no account pool yet, so calls use
//! an empty placeholder credential (mock ignores it); real providers will
//! receive a decrypted account credential in a later stage.

mod anthropic;
mod catalog;
mod logging;
#[cfg(test)]
mod model_routes_tests;
mod openai;
mod responses;
#[cfg(test)]
mod session_tests;
mod sse;
#[cfg(test)]
mod tests;
mod wire;
#[cfg(test)]
mod wire_tests;

use std::sync::Arc;

use axum::{
    extract::{rejection::JsonRejection, Request, State},
    http::{header, HeaderMap, HeaderValue},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::accounts::Credential;
use crate::config::ProviderKind;
use crate::error::{openai_error_body, ErrorKind, GatewayError};
use crate::model_service::{ModelRouteService, ResolvedModel, RouteNeeds, RouteTarget};
use crate::protocol::StreamEvent;
use crate::providers::{Capability, ChatStream, ProviderContext, ProviderRegistry};
use crate::security::{hash_token, verify_token};
use crate::session_service::{RouteLease, SessionKey, SessionRouter};

use logging::{observing_stream, LogContext};
use wire::WireRequest;

pub use catalog::{ModelCatalog, ModelEntry};
pub use openai::ChatCompletionRequest;

// ---------------------------------------------------------------------------
// Gateway API keys
// ---------------------------------------------------------------------------

/// Accepted gateway keys, stored as SHA-256 hashes.
///
/// Comparison is constant time per key and does not short-circuit on a match,
/// so verification time does not reveal which key matched.
#[derive(Clone, Default)]
pub struct ApiKeyAuth {
    hashes: Arc<Vec<String>>,
}

impl ApiKeyAuth {
    pub fn new(keys: impl IntoIterator<Item = String>) -> Result<Self, GatewayError> {
        let mut hashes = Vec::new();
        for key in keys {
            let key = key.trim();
            if key.is_empty() {
                return Err(GatewayError::config("gateway API keys must not be empty"));
            }
            hashes.push(hash_token(key));
        }
        if hashes.is_empty() {
            return Err(GatewayError::config(
                "at least one gateway API key is required",
            ));
        }
        Ok(Self {
            hashes: Arc::new(hashes),
        })
    }

    /// Constant-time verification against every configured key.
    pub fn verify(&self, presented: &str) -> bool {
        if presented.is_empty() {
            return false;
        }
        self.hashes.iter().fold(false, |matched, hash| {
            verify_token(presented, hash) | matched
        })
    }
}

/// Extract a presented key. A malformed `Authorization` header rejects the
/// request instead of falling back to `x-api-key`.
fn presented_key(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .or_else(|| value.strip_prefix("BEARER "))?;
        let token = token.trim();
        return (!token.is_empty()).then_some(token);
    }
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Shared API state.
#[derive(Clone)]
pub struct ApiState {
    registry: Arc<ProviderRegistry>,
    catalog: Arc<ModelCatalog>,
    auth: Arc<ApiKeyAuth>,
    router: Option<SessionRouter>,
    /// Persisted model aliases. Without it only native catalogue ids resolve.
    routes: Option<Arc<ModelRouteService>>,
    storage: Option<crate::storage::Storage>,
}

impl ApiState {
    pub fn new(registry: Arc<ProviderRegistry>, catalog: ModelCatalog, auth: ApiKeyAuth) -> Self {
        Self {
            registry,
            catalog: Arc::new(catalog),
            auth: Arc::new(auth),
            router: None,
            routes: None,
            storage: None,
        }
    }

    /// Attach account routing. Without it, chat calls still use a placeholder
    /// credential (mock-only behaviour used by API tests).
    pub fn with_router(mut self, router: SessionRouter) -> Self {
        self.router = Some(router);
        self
    }

    /// Attach persisted model aliases and request logging storage.
    pub fn with_model_routes(
        mut self,
        routes: Arc<ModelRouteService>,
        storage: crate::storage::Storage,
    ) -> Self {
        self.routes = Some(routes);
        self.storage = Some(storage);
        self
    }

    /// Discover models from the enabled providers and build the state.
    pub async fn discover(
        registry: Arc<ProviderRegistry>,
        keys: impl IntoIterator<Item = String>,
    ) -> Result<Self, GatewayError> {
        let credential = Credential::from_value(json!({}));
        let catalog = ModelCatalog::discover(&registry, &credential).await;
        Ok(Self::new(registry, catalog, ApiKeyAuth::new(keys)?))
    }
}

pub fn router(state: ApiState) -> Router {
    let protected = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(create_response))
        .route("/v1/messages", post(create_message))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ));
    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}

async fn require_api_key(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    match presented_key(request.headers()) {
        Some(key) if state.auth.verify(key) => next.run(request).await,
        _ => error_response(&GatewayError::auth_invalid(
            "a valid gateway API key is required",
        )),
    }
}

fn error_response(err: &GatewayError) -> Response {
    let status = err.kind.http_status();
    let mut response = (status, Json(openai_error_body(err))).into_response();
    if let Some(seconds) = err.retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": crate::VERSION }))
}

async fn list_models(State(state): State<ApiState>) -> Response {
    let mut data = Vec::new();
    if let Some(routes) = &state.routes {
        if let Ok(views) =
            crate::model_service::list_aliases(routes.storage(), &state.registry).await
        {
            for view in views {
                if let Some(item) = crate::model_service::alias_model_json(&view, &state.registry) {
                    data.push(item);
                }
            }
        }
    }
    for entry in state.catalog.enabled(&state.registry) {
        let Some(provider) = state.registry.get(&entry.provider) else {
            continue;
        };
        let info = provider.info();
        data.push(json!({
            "id": entry.id,
            "object": "model",
            "created": 0,
            "owned_by": entry.provider.to_string(),
            "display_name": entry.display_name.clone().unwrap_or_else(|| entry.id.clone()),
            "context_window": entry.context_window,
            "polygate": {
                "provider": entry.provider.to_string(),
                "kind": entry.provider.kind.as_str(),
                "region": entry.provider.region.as_str(),
                "mock": info.is_mock(),
                "test_model": info.is_mock(),
                "verification": info.verification.as_str(),
                "capabilities": {
                    "vision": entry.supports_vision
                        || crate::model_service::verified_vision(
                            entry.provider.kind,
                            entry.provider.region,
                            &entry.id,
                        ),
                },
            }
        }));
    }
    Json(json!({ "object": "list", "data": data })).into_response()
}

async fn chat_completions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(json) => json,
        Err(rejection) => {
            return error_response(&GatewayError::bad_request(format!(
                "invalid request body: {}",
                rejection.body_text()
            )))
        }
    };
    match handle_wire(&state, &headers, request).await {
        Ok(response) => response,
        Err(err) => error_response(&err),
    }
}

async fn create_response(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: Result<Json<responses::ResponsesRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(json) => json,
        Err(rejection) => {
            return error_response(&GatewayError::bad_request(format!(
                "invalid request body: {}",
                rejection.body_text()
            )))
        }
    };
    match handle_wire(&state, &headers, request).await {
        Ok(response) => response,
        Err(err) => error_response(&err),
    }
}

async fn create_message(
    State(state): State<ApiState>,
    headers: HeaderMap,
    payload: Result<Json<anthropic::AnthropicRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(json) => json,
        Err(rejection) => {
            return anthropic_error_response(&GatewayError::bad_request(format!(
                "invalid request body: {}",
                rejection.body_text()
            )))
        }
    };
    match handle_wire(&state, &headers, request).await {
        Ok(response) => response,
        Err(err) => anthropic_error_response(&err),
    }
}

fn anthropic_error_response(err: &GatewayError) -> Response {
    let status = err.kind.http_status();
    let mut response = (status, Json(crate::error::anthropic_error_body(err))).into_response();
    if let Some(seconds) = err.retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

const MAX_DISPATCH_ATTEMPTS: usize = 3;

/// Shared pipeline for every wire API: validate → resolve model → dispatch.
async fn handle_wire<W: WireRequest>(
    state: &ApiState,
    headers: &HeaderMap,
    request: W,
) -> Result<Response, GatewayError> {
    request.validate()?;
    let streaming = request.streaming();
    let needs = request.route_needs();
    // Resolve before touching any provider: an unknown or ambiguous model must
    // not cause an upstream call.
    let resolved = match resolve_model(state, request.model(), needs).await {
        Ok(resolved) => resolved,
        Err(error) => {
            if let Some(storage) = state.storage.as_ref() {
                LogContext {
                    storage: storage.clone(),
                    request_id: Uuid::new_v4().to_string(),
                    provider: None,
                    provider_kind: "unknown".into(),
                    region: None,
                    account_id: None,
                    model_alias: request.model().to_string(),
                    upstream_model: String::new(),
                    api: request.api_name(),
                    stream: streaming,
                    started: std::time::Instant::now(),
                }
                .write(error.kind.http_status().as_u16(), Some(error.kind), 0, 0)
                .await;
            }
            return Err(error);
        }
    };
    dispatch(state, headers, &request, &resolved, streaming).await
}

async fn resolve_model(
    state: &ApiState,
    name: &str,
    needs: RouteNeeds,
) -> Result<ResolvedModel, GatewayError> {
    match &state.routes {
        Some(service) => {
            service
                .resolve(&state.registry, &state.catalog, name, needs)
                .await
        }
        None => legacy_resolve(state, name, needs),
    }
}

/// Pre-alias behaviour for API tests that build `ApiState` without storage.
fn legacy_resolve(
    state: &ApiState,
    name: &str,
    needs: RouteNeeds,
) -> Result<ResolvedModel, GatewayError> {
    let entry = state
        .catalog
        .resolve(&state.registry, name.trim())
        .ok_or_else(|| {
            GatewayError::model_unavailable(format!("model `{name}` is not available"))
        })?;
    let declared = state
        .registry
        .capabilities(&entry.provider)
        .unwrap_or_default();
    let mut capabilities = crate::model_service::RouteCapabilities::default().effective(&declared);
    capabilities.vision |= entry.supports_vision
        || crate::model_service::verified_vision(
            entry.provider.kind,
            entry.provider.region,
            &entry.id,
        );
    if let Some(missing) = needs.missing(&capabilities) {
        return Err(GatewayError::bad_request(format!(
            "model `{name}` does not support {missing}"
        )));
    }
    let provider_id = state
        .registry
        .get(&entry.provider)
        .map(|info| info.info().id)
        .unwrap_or_else(|| entry.provider.to_string());
    Ok(ResolvedModel {
        exposed_id: entry.id.clone(),
        is_alias: false,
        targets: vec![RouteTarget {
            route_id: None,
            provider: entry.provider,
            provider_id,
            upstream_id: entry.upstream_id.clone(),
            display_name: entry.display_name.clone(),
            capabilities,
        }],
    })
}

fn route_needs(request: &ChatCompletionRequest) -> RouteNeeds {
    let tools = request
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
        || request.tool_choice.is_some();
    let vision = request.messages.iter().any(|message| {
        message
            .content
            .as_ref()
            .map(wire::content_has_image)
            .unwrap_or(false)
    });
    RouteNeeds {
        streaming: request.streaming(),
        tools,
        vision,
    }
}

/// Send one internal request through the resolved targets.
///
/// Retry policy: an automatic retry is only allowed while nothing has been
/// sent to the client and the failure class is safe to repeat (auth rejection,
/// 402/429, local no-capacity, model mismatch). Transport failures, timeouts
/// and 5xx are *uncertain* — the upstream may have executed the request — so
/// they are returned instead of duplicated.
async fn dispatch<W: WireRequest>(
    state: &ApiState,
    headers: &HeaderMap,
    request: &W,
    resolved: &ResolvedModel,
    streaming: bool,
) -> Result<Response, GatewayError> {
    let started = std::time::Instant::now();
    let session_id = session_id_for(headers, request.model(), request.first_user_text())
        .unwrap_or_else(|| format!("auto-{}", Uuid::new_v4().simple()));
    let capability = if streaming {
        Capability::Streaming
    } else {
        Capability::Chat
    };
    let mut excluded: Vec<String> = Vec::new();
    let mut attempts = 0usize;
    let mut last_error: Option<GatewayError> = None;
    let mut final_log: Option<LogContext> = None;

    for target in &resolved.targets {
        let provider = match state
            .registry
            .require_capability(&target.provider, capability)
        {
            Ok(provider) => provider,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        loop {
            if attempts >= MAX_DISPATCH_ATTEMPTS {
                break;
            }
            attempts += 1;
            let route = match (&state.router, target.provider.kind) {
                (Some(router), kind) if kind != ProviderKind::Mock => {
                    let key = SessionKey::new(session_id.clone(), kind, target.provider.region);
                    match router.open_excluding(key, &excluded).await {
                        Ok(route) => Some(route),
                        Err(error) => {
                            // Keep the upstream failure that triggered the
                            // retry: "no other account" is only a consequence.
                            if last_error.is_none() || !matches!(error.kind, ErrorKind::NoCapacity)
                            {
                                last_error = Some(error);
                            }
                            break;
                        }
                    }
                }
                _ => None,
            };
            let (credential, context) = match &route {
                Some(route) => (route.credential().clone(), route.provider_context()),
                None => (
                    Credential::from_value(json!({})),
                    ProviderContext::default(),
                ),
            };
            let log = state.storage.as_ref().map(|storage| LogContext {
                storage: storage.clone(),
                request_id: Uuid::new_v4().to_string(),
                provider: Some(target.provider_id.clone()),
                provider_kind: target.provider.kind.as_str().to_string(),
                region: Some(target.provider.region.as_str().to_string()),
                account_id: route.as_ref().map(|route| route.account_id().to_string()),
                model_alias: resolved.exposed_id.clone(),
                upstream_model: target.upstream_id.clone(),
                api: request.api_name(),
                stream: streaming,
                started,
            });
            final_log = log.clone();
            let internal =
                match request.to_chat_request(&target.upstream_id, Uuid::new_v4().to_string()) {
                    Ok(internal) => internal,
                    Err(error) => {
                        if let Some(route) = route {
                            fail_route(state, Some(route), &error).await;
                        }
                        return Err(error);
                    }
                };

            if streaming {
                match provider
                    .chat_stream_with_context(&context, &credential, internal)
                    .await
                {
                    Ok(stream) => {
                        let stream = match (state.router.clone(), route) {
                            (Some(router), Some(route)) => leased_stream(stream, router, route),
                            _ => stream,
                        };
                        let stream = match log {
                            Some(log) => observing_stream(stream, log),
                            None => stream,
                        };
                        let translator = request.translator(resolved.exposed_id.clone());
                        return Ok(Sse::new(sse::to_sse(stream, translator)).into_response());
                    }
                    Err(error) => {
                        let account = route.as_ref().map(|route| route.account_id().to_string());
                        let again = retry_same_target(&error);
                        fail_route(state, route, &error).await;
                        exclude_account(&mut excluded, account);
                        last_error = Some(error);
                        if again {
                            continue;
                        }
                        break;
                    }
                }
            } else {
                match provider
                    .chat_with_context(&context, &credential, internal)
                    .await
                {
                    Ok(response) => {
                        if let (Some(router), Some(route)) = (&state.router, route) {
                            router.finish(route).await;
                        }
                        let (input, output) = response
                            .usage
                            .map(|usage| (usage.input_tokens as i64, usage.output_tokens as i64))
                            .unwrap_or((0, 0));
                        let body = match request.render(&response, &resolved.exposed_id) {
                            Ok(body) => body,
                            Err(error) => {
                                if let Some(log) = log {
                                    log.write(
                                        error.kind.http_status().as_u16(),
                                        Some(error.kind),
                                        input,
                                        output,
                                    )
                                    .await;
                                }
                                return Err(error);
                            }
                        };
                        if let Some(log) = log {
                            log.write(200, None, input, output).await;
                        }
                        record_usage(state, target, &resolved.exposed_id, &response, "ok").await;
                        return Ok(Json(body).into_response());
                    }
                    Err(error) => {
                        let account = route.as_ref().map(|route| route.account_id().to_string());
                        let again = retry_same_target(&error);
                        fail_route(state, route, &error).await;
                        exclude_account(&mut excluded, account);
                        last_error = Some(error);
                        if again {
                            continue;
                        }
                        break;
                    }
                }
            }
        }
    }

    let error = last_error.unwrap_or_else(|| {
        GatewayError::model_unavailable(format!(
            "no route target for `{}` could serve the request",
            resolved.exposed_id
        ))
    });
    if let Some(log) = final_log {
        log.write(error.kind.http_status().as_u16(), Some(error.kind), 0, 0)
            .await;
    }
    Err(error)
}

fn exclude_account(excluded: &mut Vec<String>, account: Option<String>) {
    if let Some(account) = account {
        if !excluded.contains(&account) {
            excluded.push(account);
        }
    }
}

/// Failures that another *account* of the same provider may not share. A model
/// mismatch is target-level: it moves to the next configured target instead.
fn retry_same_target(error: &GatewayError) -> bool {
    matches!(
        error.kind,
        ErrorKind::AuthExpired
            | ErrorKind::AuthInvalid
            | ErrorKind::AccountDisabled
            | ErrorKind::ChannelRestricted
            | ErrorKind::RateLimited
            | ErrorKind::QuotaExhausted
            | ErrorKind::NoCapacity
    )
}

/// Best-effort request accounting; a logging failure never fails the request.
async fn record_usage(
    state: &ApiState,
    target: &RouteTarget,
    exposed_model: &str,
    response: &crate::protocol::ChatResponse,
    status: &str,
) {
    let Some(storage) = state.storage.as_ref() else {
        return;
    };
    let (input, output) = response
        .usage
        .map(|usage| (usage.input_tokens as i64, usage.output_tokens as i64))
        .unwrap_or((0, 0));
    let event = crate::storage::NewUsageEvent {
        request_id: Uuid::new_v4().to_string(),
        provider_kind: target.provider.kind.as_str().to_string(),
        account_id: None,
        model: Some(exposed_model.to_string()),
        input_tokens: input,
        output_tokens: output,
        status: status.to_string(),
    };
    if let Err(error) = storage.usage().record(&event).await {
        tracing::warn!(error = %error.message, "could not record usage");
    }
}

async fn fail_route(state: &ApiState, route: Option<RouteLease>, error: &GatewayError) {
    if let (Some(router), Some(route)) = (&state.router, route) {
        router.report_failure(&route, error).await;
        router.finish(route).await;
    }
}

/// Keep the session/account permits for the entire SSE body. A terminal error
/// triggers the same recovery policy as a pre-stream error; EOF (or a dropped
/// client) releases the permits.
fn leased_stream(inner: ChatStream, router: SessionRouter, route: RouteLease) -> ChatStream {
    struct State {
        inner: ChatStream,
        router: SessionRouter,
        route: Option<RouteLease>,
        recovered: bool,
    }

    let state = State {
        inner,
        router,
        route: Some(route),
        recovered: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        match state.inner.next().await {
            Some(Ok(event)) => {
                if let StreamEvent::Error { error } = &event {
                    if !state.recovered {
                        state.recovered = true;
                        if let Some(route) = state.route.take() {
                            let gateway_error =
                                GatewayError::new(error.kind, error.message.clone());
                            state.router.report_failure(&route, &gateway_error).await;
                            state.router.finish(route).await;
                        }
                    }
                }
                Some((Ok(event), state))
            }
            Some(Err(error)) => {
                if !state.recovered {
                    state.recovered = true;
                    if let Some(route) = state.route.take() {
                        state.router.report_failure(&route, &error).await;
                        state.router.finish(route).await;
                    }
                }
                Some((Err(error), state))
            }
            None => {
                if let Some(route) = state.route.take() {
                    state.router.finish(route).await;
                }
                None
            }
        }
    }))
}

/// Stable downstream session id: an explicit header wins; otherwise the first
/// user message plus the public model id gives a deterministic conversation
/// prefix (the CodeArts client has no session header of its own).
fn session_id_for(headers: &HeaderMap, model: &str, text: Option<String>) -> Option<String> {
    if let Some(id) = crate::sessions::session_id_from_headers(headers) {
        return Some(id);
    }
    let text = text?;
    if text.trim().is_empty() {
        return None;
    }
    let digest = Sha256::new_with_prefix(format!("{model}:{text}").as_bytes()).finalize();
    Some(format!("auto-{}", &hex::encode(digest)[..24]))
}
