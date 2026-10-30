//! Provider contract: capability declaration, verification state and the
//! dynamic `(kind, region)` registry.
//!
//! A provider is one upstream platform *instance*: the same platform in two
//! regions is two independent providers with separate credentials, quota and
//! model catalogues. Capabilities are declared explicitly and default to
//! `false`; a capability bit never substitutes for real upstream verification.
//!
//! Dependency boundary: this module may use `protocol`, `error`, read-only
//! `accounts::{Credential, AuthStatus}` and the `quota::QuotaSnapshot` domain
//! struct. It must not touch `storage`, `scheduler`, `api` or concrete provider
//! implementations.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_stream::Stream;

use crate::accounts::{AuthStatus, Credential};
use crate::config::{ProviderKind, Region, Verification};
use crate::error::GatewayError;
use crate::protocol::{ChatRequest, ChatResponse, StreamEvent};
use crate::quota::QuotaSnapshot;

mod mock;
#[cfg(test)]
mod mock_tests;
mod registry;
#[cfg(test)]
mod tests;

pub mod codearts;
pub mod loomy;
pub mod officeace;
pub mod profile;
pub mod qoder;
pub mod trae;
pub mod workbuddy;

pub use codearts::{ClientHeaders, CodeArtsHttp, CodeArtsProvider};
pub use loomy::LoomyProvider;
pub use officeace::OfficeAceProvider;
pub use trae::provider::TraeProvider;
pub use workbuddy::provider::WorkbuddyProvider;

pub use mock::{
    FaultInjection, FaultMode, FaultScope, MockConfig, MockProvider, MockReply, MockToolCall,
};
pub use registry::ProviderRegistry;

/// Shared handle used by the registry and callers.
pub type ProviderRef = Arc<dyn Provider>;

/// One item of a streamed chat completion.
pub type StreamItem = Result<StreamEvent, GatewayError>;

/// Boxed event stream returned by [`Provider::chat_stream`].
pub type ChatStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;

// ---------------------------------------------------------------------------
// Request context
// ---------------------------------------------------------------------------

/// Gateway-side context for one provider call.
///
/// This is not part of the wire protocol. It tells an adapter which account it
/// is acting for and which upstream session id the session binding selected.
/// Adapters must not forward these values upstream except where the upstream
/// protocol has a documented field (CodeArts `chat_id` is such a field).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderContext {
    /// Account whose credential was decrypted for this call.
    pub account_id: Option<String>,
    /// Credential revision observed before the call (for compare-and-swap
    /// persistence after a refresh).
    pub credential_revision: Option<i64>,
    /// Downstream client session id, when the caller supplied a stable one.
    pub client_session_id: Option<String>,
    /// Upstream session id bound to that client session. Adapters that have a
    /// native session concept use it verbatim; others ignore it.
    pub upstream_session_id: Option<String>,
}

/// Persistence boundary used by an adapter that has to rotate a credential
/// (for example an expiring STS token with a one-time refresh token).
///
/// Implemented by `account_service`; defined here so provider adapters never
/// link against storage or take a dependency on the account service crate.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Replace a credential only if the caller observed `expected_revision`.
    /// Returns the new revision. A conflict is an error; the caller reloads and
    /// decides whether the stored credential is already fresh.
    async fn update_credential(
        &self,
        account_id: &str,
        expected_revision: i64,
        credential: &Credential,
    ) -> Result<i64, GatewayError>;

    /// Load the current credential and revision.
    async fn load_credential(
        &self,
        account_id: &str,
    ) -> Result<Option<(Credential, i64)>, GatewayError>;

    /// Try to take a short cross-process lease before calling an upstream
    /// refresh endpoint. The default implementation grants immediately: an
    /// adapter that shares one database between processes overrides this.
    async fn acquire_refresh_lease(
        &self,
        _account_id: &str,
        _owner: &str,
        _now: i64,
        _ttl_seconds: i64,
    ) -> Result<bool, GatewayError> {
        Ok(true)
    }

    /// Release a lease only if `owner` still holds it.
    async fn release_refresh_lease(
        &self,
        _account_id: &str,
        _owner: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Registry key. Region is part of the identity: `qoder-cn` and `qoder-intl`
/// are different providers and never share credentials or quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderKey {
    pub kind: ProviderKind,
    pub region: Region,
}

impl ProviderKey {
    pub fn new(kind: ProviderKind, region: Region) -> Self {
        Self { kind, region }
    }
}

impl From<(ProviderKind, Region)> for ProviderKey {
    fn from((kind, region): (ProviderKind, Region)) -> Self {
        Self { kind, region }
    }
}

impl fmt::Display for ProviderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.kind.as_str(), self.region.as_str())
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// A single declared capability. Used for gating and for admin/inspection
/// output; an unset bit means "not implemented", not "temporarily broken".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Credential can be validated against the upstream.
    Authentication,
    /// Models can be discovered from the upstream.
    ModelDiscovery,
    /// Remaining balance can be queried.
    Quota,
    /// Non-streaming chat completion.
    Chat,
    /// Streaming chat completion.
    Streaming,
    /// Tool/function calling in either mode.
    ToolCalling,
    /// Provider-native session resume (not the gateway's own affinity).
    NativeSession,
    /// Long running agent tasks.
    AgentTask,
    /// In-flight request/task cancellation.
    Cancellation,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Capability::Authentication,
        Capability::ModelDiscovery,
        Capability::Quota,
        Capability::Chat,
        Capability::Streaming,
        Capability::ToolCalling,
        Capability::NativeSession,
        Capability::AgentTask,
        Capability::Cancellation,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::Authentication => "authentication",
            Capability::ModelDiscovery => "model_discovery",
            Capability::Quota => "quota",
            Capability::Chat => "chat",
            Capability::Streaming => "streaming",
            Capability::ToolCalling => "tool_calling",
            Capability::NativeSession => "native_session",
            Capability::AgentTask => "agent_task",
            Capability::Cancellation => "cancellation",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Declared capabilities of one provider instance.
///
/// Every bit is `false` by default: a provider must opt in explicitly. The
/// declaration is a claim about the implementation, not proof that the live
/// upstream accepts it — that is what [`VerificationState`] records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub authentication: bool,
    pub model_discovery: bool,
    pub quota: bool,
    pub chat: bool,
    pub streaming: bool,
    pub tools: bool,
    pub native_session: bool,
    pub agent_task: bool,
    pub cancellation: bool,
}

impl Capabilities {
    pub fn get(&self, capability: Capability) -> bool {
        match capability {
            Capability::Authentication => self.authentication,
            Capability::ModelDiscovery => self.model_discovery,
            Capability::Quota => self.quota,
            Capability::Chat => self.chat,
            Capability::Streaming => self.streaming,
            Capability::ToolCalling => self.tools,
            Capability::NativeSession => self.native_session,
            Capability::AgentTask => self.agent_task,
            Capability::Cancellation => self.cancellation,
        }
    }

    pub fn set(&mut self, capability: Capability, value: bool) {
        match capability {
            Capability::Authentication => self.authentication = value,
            Capability::ModelDiscovery => self.model_discovery = value,
            Capability::Quota => self.quota = value,
            Capability::Chat => self.chat = value,
            Capability::Streaming => self.streaming = value,
            Capability::ToolCalling => self.tools = value,
            Capability::NativeSession => self.native_session = value,
            Capability::AgentTask => self.agent_task = value,
            Capability::Cancellation => self.cancellation = value,
        }
    }

    /// Names of every declared capability, in a stable order.
    pub fn declared(&self) -> Vec<&'static str> {
        Capability::ALL
            .iter()
            .filter(|c| self.get(**c))
            .map(|c| c.as_str())
            .collect()
    }

    /// Classify the provider. `None` means it declares neither inference nor
    /// agent tasks, so the gateway cannot route user work to it.
    pub fn class(&self) -> Option<ProviderClass> {
        match (self.chat, self.agent_task) {
            (true, true) => Some(ProviderClass::Hybrid),
            (true, false) => Some(ProviderClass::Model),
            (false, true) => Some(ProviderClass::Agent),
            (false, false) => None,
        }
    }
}

/// Shape of the work a provider accepts. Coding agents are not assumed to offer
/// plain LLM inference, and agent tasks are not translated into chat semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// Chat/model inference only.
    Model,
    /// Agent tasks only.
    Agent,
    /// Both.
    Hybrid,
}

impl ProviderClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderClass::Model => "model",
            ProviderClass::Agent => "agent",
            ProviderClass::Hybrid => "hybrid",
        }
    }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// How far the provider has been validated against the real upstream.
///
/// Only an end-to-end run with a real account may promote a provider to
/// `Verified`; mock providers stay `Experimental` forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// Real credentials exercised end to end.
    Verified,
    /// Implemented from protocol research, not exercised end to end.
    Experimental,
    /// The platform or this implementation does not support it.
    Unsupported,
    /// Could not be validated (no credentials, region unreachable).
    Unavailable,
}

impl VerificationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationState::Verified => "verified",
            VerificationState::Experimental => "experimental",
            VerificationState::Unsupported => "unsupported",
            VerificationState::Unavailable => "unavailable",
        }
    }

    pub fn is_verified(&self) -> bool {
        matches!(self, VerificationState::Verified)
    }

    /// Whether requests may be routed here. `Unsupported`/`Unavailable` are
    /// configuration or platform states, never routing candidates.
    pub fn routable(&self) -> bool {
        matches!(
            self,
            VerificationState::Verified | VerificationState::Experimental
        )
    }
}

impl From<Verification> for VerificationState {
    fn from(v: Verification) -> Self {
        match v {
            Verification::Verified => VerificationState::Verified,
            // A local mock is never a verified real provider; mock-ness is
            // carried by `ProviderKind::Mock` and `ProviderInfo::is_mock`.
            Verification::Mock | Verification::Experimental => VerificationState::Experimental,
            Verification::Unsupported => VerificationState::Unsupported,
            Verification::Unavailable => VerificationState::Unavailable,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider metadata and models
// ---------------------------------------------------------------------------

/// Static, immutable description of one provider instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// Instance id, e.g. `qoder-cn`; unique across the registry.
    pub id: String,
    pub kind: ProviderKind,
    pub region: Region,
    pub display_name: String,
    /// Implementation version reported to clients and the admin UI.
    pub version: String,
    pub verification: VerificationState,
    /// Initial enabled state. The registry is authoritative at runtime and
    /// overlays its own flag when listing.
    pub enabled: bool,
}

impl ProviderInfo {
    pub fn key(&self) -> ProviderKey {
        ProviderKey::new(self.kind, self.region)
    }

    /// Mock providers are never advertised as real upstreams.
    pub fn is_mock(&self) -> bool {
        self.kind == ProviderKind::Mock
    }
}

/// A model as reported by the upstream. Unified routing ids and aliases are
/// applied by `models::ModelRegistry`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamModel {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
}

impl UpstreamModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
            max_output: None,
        }
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
}

/// An agent task submitted to providers that support `agent_task`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskRequest {
    /// Client-visible task id, stable for the lifetime of the task.
    pub task_id: String,
    pub model: String,
    pub instructions: String,
    /// Scratch directory the agent may work in, if the client supplied one.
    pub workspace: Option<String>,
    pub metadata: serde_json::Value,
}

impl AgentTaskRequest {
    pub fn new(
        task_id: impl Into<String>,
        model: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            model: model.into(),
            instructions: instructions.into(),
            workspace: None,
            metadata: serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub task_id: String,
    pub state: AgentTaskState,
    /// Upstream task handle required to poll or cancel it.
    pub upstream_task_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

impl AgentTask {
    pub fn new(task_id: impl Into<String>, state: AgentTaskState) -> Self {
        Self {
            task_id: task_id.into(),
            state,
            upstream_task_id: None,
            output: None,
            error: None,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Cancel a streaming response or an agent task that is still in flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    /// Gateway-side request/task id.
    pub request_id: String,
    /// Upstream handle, when the provider assigned one.
    pub upstream_request_id: Option<String>,
    pub reason: Option<String>,
}

impl CancelRequest {
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            upstream_request_id: None,
            reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// One upstream platform instance.
///
/// Only [`info`](Provider::info) and [`capabilities`](Provider::capabilities)
/// are required. Every operation defaults to `ErrorKind::Unsupported`, so a
/// provider that does not implement something fails explicitly instead of
/// pretending to succeed.
#[async_trait]
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    fn capabilities(&self) -> Capabilities;

    async fn auth_status(&self, _credential: &Credential) -> Result<AuthStatus, GatewayError> {
        Err(unsupported_capability(
            &self.info(),
            Capability::Authentication,
        ))
    }

    /// Context-aware auth probe. Defaults to [`Provider::auth_status`]; adapters
    /// that may need to refresh an expiring credential override this so the
    /// refreshed value can be persisted against the account.
    async fn auth_status_with_context(
        &self,
        _context: &ProviderContext,
        credential: &Credential,
    ) -> Result<AuthStatus, GatewayError> {
        self.auth_status(credential).await
    }

    async fn list_models(
        &self,
        _credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        Err(unsupported_capability(
            &self.info(),
            Capability::ModelDiscovery,
        ))
    }

    async fn quota(&self, _credential: &Credential) -> Result<QuotaSnapshot, GatewayError> {
        Err(unsupported_capability(&self.info(), Capability::Quota))
    }

    /// Context-aware quota probe. Defaults to [`Provider::quota`]; adapters that
    /// refresh an expiring credential before probing override this so the probe
    /// does not fail with a dead token.
    async fn quota_with_context(
        &self,
        _context: &ProviderContext,
        credential: &Credential,
    ) -> Result<QuotaSnapshot, GatewayError> {
        self.quota(credential).await
    }

    async fn chat(
        &self,
        _credential: &Credential,
        _request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        Err(unsupported_capability(&self.info(), Capability::Chat))
    }

    async fn chat_stream(
        &self,
        _credential: &Credential,
        _request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        Err(unsupported_capability(&self.info(), Capability::Streaming))
    }

    /// Context-aware non-streaming chat. Defaults to [`Provider::chat`] for
    /// adapters that have no account/session state.
    async fn chat_with_context(
        &self,
        _context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        self.chat(credential, request).await
    }

    /// Context-aware streaming chat. Defaults to [`Provider::chat_stream`].
    async fn chat_stream_with_context(
        &self,
        _context: &ProviderContext,
        credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        self.chat_stream(credential, request).await
    }

    async fn agent_task(&self, _request: AgentTaskRequest) -> Result<AgentTask, GatewayError> {
        Err(unsupported_capability(&self.info(), Capability::AgentTask))
    }

    async fn cancel(&self, _request: CancelRequest) -> Result<(), GatewayError> {
        Err(unsupported_capability(
            &self.info(),
            Capability::Cancellation,
        ))
    }
}

impl fmt::Debug for dyn Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let info = self.info();
        f.debug_struct("Provider")
            .field("id", &info.id)
            .field("kind", &info.kind)
            .field("region", &info.region)
            .field("verification", &info.verification)
            .finish_non_exhaustive()
    }
}

/// Classified "not implemented here" error, safe to return to a client.
pub fn unsupported_capability(info: &ProviderInfo, capability: Capability) -> GatewayError {
    GatewayError::unsupported(format!(
        "provider {} ({}) does not support {}",
        info.id,
        info.key(),
        capability
    ))
}
