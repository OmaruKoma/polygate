//! Deterministic mock provider for development and tests.
//!
//! The mock never talks to a network. Responses are derived only from the
//! request and the [`MockConfig`], so identical input and configuration produce
//! identical content, usage and finish reasons. Request ordinals (used in
//! response ids and fault targeting) are per-instance counters.
//!
//! Fault injection is deterministic too: a [`FaultInjection`] with `at = None`
//! applies to every request, with `at = Some(n)` only to the n-th request
//! (0-based, counted per provider instance). Stream faults apply after
//! `after_events` events; `FaultMode::StreamInterrupted` truncates the stream
//! without a terminator instead of emitting an `Error` event.
//!
//! Cancellation uses plain Rust lifetimes: the stream owns its generator state
//! and has no background task, so dropping it stops generation. Dropping the
//! stream also removes its entry from the small per-instance cancel registry,
//! and [`Provider::cancel`] stops a stream that is still alive.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::accounts::Credential;
use crate::config::{ProviderKind, Region};
use crate::error::{ErrorKind, GatewayError};
use crate::protocol::{
    ChatMessage, ChatRequest, ChatResponse, ContentBlock, FinishReason, MessageRole, StreamError,
    StreamEvent, ToolCall, Usage,
};

use super::{
    Capabilities, ChatStream, Provider, ProviderInfo, ProviderRef, StreamItem, UpstreamModel,
    VerificationState,
};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// One tool invocation the mock should produce.
#[derive(Debug, Clone, PartialEq)]
pub struct MockToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl MockToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// Serialised arguments, stable for a given [`serde_json::Value`].
    pub fn arguments_json(&self) -> String {
        serde_json::to_string(&self.arguments).unwrap_or_else(|_| "{}".into())
    }
}

/// How the mock words its text reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockReply {
    /// Constants text, independent of the request.
    Fixed(String),
    /// Echo the last user message, prefixed.
    Echo { prefix: String },
}

impl MockReply {
    pub fn fixed(text: impl Into<String>) -> Self {
        MockReply::Fixed(text.into())
    }

    pub fn echo(prefix: impl Into<String>) -> Self {
        MockReply::Echo {
            prefix: prefix.into(),
        }
    }

    pub fn render(&self, request: &ChatRequest) -> String {
        match self {
            MockReply::Fixed(text) => text.clone(),
            MockReply::Echo { prefix } => {
                let last = request
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == MessageRole::User)
                    .map(ChatMessage::text_content)
                    .unwrap_or_default();
                if last.is_empty() {
                    format!("{prefix}<no user message>")
                } else {
                    format!("{prefix}{last}")
                }
            }
        }
    }
}

/// Failure the mock should behave as if the upstream produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    /// Upstream rejected the credential (gateway 401).
    AuthInvalid,
    /// Upstream reports exhausted quota (gateway 402).
    QuotaExhausted,
    /// Upstream rate limited the request (gateway 429).
    RateLimited,
    /// Upstream returned a 5xx (gateway 502, recorded upstream status 500).
    UpstreamServer,
    /// Upstream did not answer in time (gateway 504).
    Timeout,
    /// Request was cancelled (gateway 499).
    Cancelled,
    /// Connection failure (gateway 502).
    Transport,
    /// Stream ends mid-flight without a terminator.
    StreamInterrupted,
}

impl FaultMode {
    /// Error kind used for a reported failure. Interruption maps to
    /// `UpstreamServer` for non-streaming calls, which have no truncation
    /// representation; streaming truncates instead of reporting this error.
    fn error_kind(self) -> ErrorKind {
        match self {
            FaultMode::AuthInvalid => ErrorKind::AuthInvalid,
            FaultMode::QuotaExhausted => ErrorKind::QuotaExhausted,
            FaultMode::RateLimited => ErrorKind::RateLimited,
            FaultMode::UpstreamServer | FaultMode::StreamInterrupted => ErrorKind::UpstreamServer,
            FaultMode::Timeout => ErrorKind::Timeout,
            FaultMode::Cancelled => ErrorKind::Cancelled,
            FaultMode::Transport => ErrorKind::Transport,
        }
    }
}

/// Which call style a fault applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultScope {
    Both,
    Chat,
    Stream,
}

/// One deterministic fault rule.
#[derive(Debug, Clone)]
pub struct FaultInjection {
    pub mode: FaultMode,
    pub scope: FaultScope,
    /// Request ordinal this fault applies to; `None` applies to every request.
    pub at: Option<u64>,
    /// Number of stream events emitted before the stream fails. Ignored by
    /// non-streaming calls.
    pub after_events: usize,
    /// Simulated wait before a `Timeout` failure is reported.
    pub timeout: Duration,
}

impl FaultInjection {
    pub fn new(mode: FaultMode) -> Self {
        Self {
            mode,
            scope: FaultScope::Both,
            at: None,
            after_events: 0,
            timeout: Duration::from_millis(10),
        }
    }

    pub fn scope(mut self, scope: FaultScope) -> Self {
        self.scope = scope;
        self
    }

    pub fn at(mut self, ordinal: u64) -> Self {
        self.at = Some(ordinal);
        self
    }

    pub fn after_events(mut self, events: usize) -> Self {
        self.after_events = events;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn applies_to(&self, ordinal: u64, streaming: bool) -> bool {
        let scope_matches = match self.scope {
            FaultScope::Both => true,
            FaultScope::Chat => !streaming,
            FaultScope::Stream => streaming,
        };
        scope_matches && (self.at.is_none() || self.at == Some(ordinal))
    }
}

/// Configuration of one mock provider instance.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub id: String,
    pub display_name: String,
    pub region: Region,
    pub version: String,
    pub enabled: bool,
    pub models: Vec<UpstreamModel>,
    pub reply: MockReply,
    /// Optional reasoning text, streamed before the visible answer.
    pub reasoning: Option<String>,
    /// Tool calls returned instead of (or in addition to) text.
    pub tool_calls: Vec<MockToolCall>,
    /// Characters per streamed chunk (text, reasoning and tool arguments).
    pub chunk_size: usize,
    /// Delay before every streamed event.
    pub event_delay: Duration,
    /// Delay before a non-streamed response.
    pub response_delay: Duration,
    pub faults: Vec<FaultInjection>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            id: "mock-cn".into(),
            display_name: "Mock (development)".into(),
            region: Region::Cn,
            version: env!("CARGO_PKG_VERSION").into(),
            enabled: true,
            models: vec![
                UpstreamModel::new("mock-chat").with_display_name("Mock Chat"),
                UpstreamModel::new("mock-reasoner").with_display_name("Mock Reasoner"),
            ],
            reply: MockReply::echo("mock: "),
            reasoning: None,
            tool_calls: Vec::new(),
            chunk_size: 8,
            event_delay: Duration::ZERO,
            response_delay: Duration::ZERO,
            faults: Vec::new(),
        }
    }
}

impl MockConfig {
    pub fn new(id: impl Into<String>, region: Region) -> Self {
        Self {
            id: id.into(),
            region,
            ..Self::default()
        }
    }

    pub fn with_reply(mut self, reply: MockReply) -> Self {
        self.reply = reply;
        self
    }

    pub fn with_reasoning(mut self, text: impl Into<String>) -> Self {
        self.reasoning = Some(text.into());
        self
    }

    pub fn with_tool_call(mut self, call: MockToolCall) -> Self {
        self.tool_calls.push(call);
        self
    }

    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    pub fn with_event_delay(mut self, delay: Duration) -> Self {
        self.event_delay = delay;
        self
    }

    pub fn with_response_delay(mut self, delay: Duration) -> Self {
        self.response_delay = delay;
        self
    }

    pub fn with_fault(mut self, fault: FaultInjection) -> Self {
        self.faults.push(fault);
        self
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Deterministic provider used for development, integration tests and the
/// zero-config default configuration. It is never `VerificationState::Verified`.
pub struct MockProvider {
    info: ProviderInfo,
    capabilities: Capabilities,
    config: MockConfig,
    models: Vec<UpstreamModel>,
    ordinal: AtomicU64,
    cancels: Arc<CancelRegistry>,
}

impl MockProvider {
    pub fn new(config: MockConfig) -> Self {
        let info = ProviderInfo {
            id: config.id.clone(),
            kind: ProviderKind::Mock,
            region: config.region,
            display_name: config.display_name.clone(),
            version: config.version.clone(),
            // A local mock is never a verified real provider.
            verification: VerificationState::Experimental,
            enabled: config.enabled,
        };
        let models = if config.models.is_empty() {
            vec![UpstreamModel::new("mock-chat")]
        } else {
            config.models.clone()
        };
        let capabilities = Capabilities {
            model_discovery: true,
            chat: true,
            streaming: true,
            tools: true,
            cancellation: true,
            ..Capabilities::default()
        };
        Self {
            info,
            capabilities,
            config,
            models,
            ordinal: AtomicU64::new(0),
            cancels: Arc::new(CancelRegistry::default()),
        }
    }

    pub fn development() -> Self {
        Self::new(MockConfig::default())
    }

    pub fn shared(config: MockConfig) -> ProviderRef {
        Arc::new(Self::new(config))
    }

    /// Streams currently registered for cancellation (0 when idle).
    pub fn in_flight_requests(&self) -> usize {
        self.cancels.len()
    }

    /// Requests seen so far. Used to prove that rejected requests never reach
    /// the provider.
    pub fn request_count(&self) -> u64 {
        self.ordinal.load(Ordering::SeqCst)
    }

    pub fn config(&self) -> &MockConfig {
        &self.config
    }

    fn next_ordinal(&self) -> u64 {
        self.ordinal.fetch_add(1, Ordering::SeqCst)
    }

    fn fault_for(&self, ordinal: u64, streaming: bool) -> Option<FaultInjection> {
        self.config
            .faults
            .iter()
            .find(|f| f.applies_to(ordinal, streaming))
            .cloned()
    }

    fn ensure_model(&self, model: &str) -> Result<(), GatewayError> {
        if self.models.iter().any(|m| m.id == model) {
            Ok(())
        } else {
            Err(GatewayError::model_unavailable(format!(
                "mock provider {} does not serve model {model}",
                self.info.id
            )))
        }
    }

    async fn injected_failure(&self, fault: &FaultInjection, streaming: bool) -> GatewayError {
        if fault.mode == FaultMode::Timeout && !fault.timeout.is_zero() {
            tokio::time::sleep(fault.timeout).await;
        }
        let message = format!(
            "mock provider {}: {}",
            self.info.id,
            fault_message(fault.mode)
        );
        match fault.mode {
            FaultMode::AuthInvalid => GatewayError::new(ErrorKind::AuthInvalid, message),
            FaultMode::QuotaExhausted => GatewayError::new(ErrorKind::QuotaExhausted, message),
            FaultMode::RateLimited => {
                GatewayError::new(ErrorKind::RateLimited, message).retry_after(1)
            }
            FaultMode::UpstreamServer => {
                GatewayError::upstream_server(message).with_upstream_status(500)
            }
            FaultMode::Timeout => GatewayError::timeout(message),
            FaultMode::Cancelled => GatewayError::new(ErrorKind::Cancelled, message),
            FaultMode::Transport => GatewayError::transport(message),
            FaultMode::StreamInterrupted => {
                let message = if streaming {
                    format!("mock provider {}: stream interrupted", self.info.id)
                } else {
                    format!("mock provider {}: response interrupted", self.info.id)
                };
                GatewayError::new(ErrorKind::UpstreamServer, message).with_upstream_status(500)
            }
        }
    }

    /// Deterministic usage derived from the request and the generated output.
    fn usage_for(&self, request: &ChatRequest, reply: &str) -> Usage {
        let input_text: String = request
            .messages
            .iter()
            .map(ChatMessage::text_content)
            .collect();
        let input_tokens = estimate_tokens(&input_text);
        let reasoning_tokens = self
            .config
            .reasoning
            .as_deref()
            .map(estimate_tokens)
            .filter(|tokens| *tokens > 0);
        let tool_tokens: u64 = self
            .config
            .tool_calls
            .iter()
            .map(|call| estimate_tokens(&call.arguments_json()))
            .sum();
        let output_tokens = estimate_tokens(reply) + reasoning_tokens.unwrap_or(0) + tool_tokens;
        Usage {
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cached_input_tokens: None,
            total_tokens: Some(input_tokens + output_tokens),
        }
    }

    fn content_blocks(&self, reply: &str) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        if let Some(reasoning) = &self.config.reasoning {
            blocks.push(ContentBlock::reasoning(reasoning.clone()));
        }
        if !reply.is_empty() {
            blocks.push(ContentBlock::text(reply.to_string()));
        }
        for call in &self.config.tool_calls {
            blocks.push(ContentBlock::ToolCall(ToolCall::new(
                call.id.clone(),
                call.name.clone(),
                call.arguments_json(),
            )));
        }
        blocks
    }

    fn finish_reason(&self) -> FinishReason {
        if self.config.tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        }
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

    async fn list_models(
        &self,
        _credential: &Credential,
    ) -> Result<Vec<UpstreamModel>, GatewayError> {
        Ok(self.models.clone())
    }

    async fn chat(
        &self,
        _credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatResponse, GatewayError> {
        let ordinal = self.next_ordinal();
        if let Some(fault) = self.fault_for(ordinal, false) {
            return Err(self.injected_failure(&fault, false).await);
        }
        self.ensure_model(&request.model)?;
        sleep_for(self.config.response_delay).await;

        let reply = self.config.reply.render(&request);
        let usage = self.usage_for(&request, &reply);
        let finish_reason = self.finish_reason();
        let mut response = ChatResponse::new(
            format!("{}-{ordinal}", self.info.id),
            request.model.clone(),
            ChatMessage::new(MessageRole::Assistant, self.content_blocks(&reply)),
            finish_reason,
        );
        response.request_id = request.request_id.clone();
        response.usage = Some(usage);
        Ok(response)
    }

    async fn chat_stream(
        &self,
        _credential: &Credential,
        request: ChatRequest,
    ) -> Result<ChatStream, GatewayError> {
        let ordinal = self.next_ordinal();
        let fault = self.fault_for(ordinal, true);
        if let Some(fault) = &fault {
            if fault.after_events == 0 {
                return Err(self.injected_failure(fault, true).await);
            }
        }
        self.ensure_model(&request.model)?;

        let request_id = request
            .request_id
            .clone()
            .unwrap_or_else(|| format!("mock-stream-{}-{ordinal}", self.info.id));
        let reply = self.config.reply.render(&request);
        let usage = self.usage_for(&request, &reply);
        let slot = self.cancels.register(&request_id);
        let registration = CancelRegistration {
            registry: Arc::clone(&self.cancels),
            id: request_id.clone(),
            slot: Arc::clone(&slot),
        };

        let state = MockStreamState {
            response_id: format!("{}-{ordinal}", self.info.id),
            request_id: request.request_id.clone(),
            model: request.model.clone(),
            chunk_size: self.config.chunk_size.max(1),
            event_delay: self.config.event_delay,
            text: reply,
            text_pos: 0,
            reasoning: self.config.reasoning.clone().unwrap_or_default(),
            reasoning_pos: 0,
            tool_calls: self
                .config
                .tool_calls
                .iter()
                .map(|call| PreparedToolCall::new(call, self.config.chunk_size))
                .collect(),
            arg_positions: vec![0; self.config.tool_calls.len()],
            args_next_call: 0,
            phase: Phase::Start,
            usage,
            finish_reason: self.finish_reason(),
            emitted: 0,
            mid_fault: fault.filter(|f| f.after_events > 0),
            fault_fired: false,
            cancel: Some(slot),
            _registration: Some(registration),
            done: false,
        };

        let events = stream::unfold(state, |mut state| async move {
            state.next().await.map(|item| (item, state))
        });
        let stream: ChatStream = Box::pin(events);
        Ok(stream)
    }

    async fn cancel(&self, request: super::CancelRequest) -> Result<(), GatewayError> {
        if self.cancels.cancel(&request.request_id) {
            Ok(())
        } else {
            Err(GatewayError::bad_request(format!(
                "mock provider {} has no in-flight request {}",
                self.info.id, request.request_id
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Start,
    Reasoning,
    Text,
    ToolStart(usize),
    Args,
    Usage,
    Finish,
    Done,
}

struct PreparedToolCall {
    id: String,
    name: String,
    fragments: Vec<String>,
}

impl PreparedToolCall {
    fn new(call: &MockToolCall, chunk_size: usize) -> Self {
        Self {
            id: call.id.clone(),
            name: call.name.clone(),
            fragments: split_chunks(&call.arguments_json(), chunk_size.max(1)),
        }
    }
}

struct MockStreamState {
    response_id: String,
    request_id: Option<String>,
    model: String,
    chunk_size: usize,
    event_delay: Duration,
    text: String,
    text_pos: usize,
    reasoning: String,
    reasoning_pos: usize,
    tool_calls: Vec<PreparedToolCall>,
    arg_positions: Vec<usize>,
    args_next_call: usize,
    phase: Phase,
    usage: Usage,
    finish_reason: FinishReason,
    emitted: usize,
    mid_fault: Option<FaultInjection>,
    fault_fired: bool,
    cancel: Option<Arc<CancelSlot>>,
    /// Kept only for its `Drop`: removes the cancel registration.
    _registration: Option<CancelRegistration>,
    done: bool,
}

impl MockStreamState {
    async fn next(&mut self) -> Option<StreamItem> {
        if self.done {
            return None;
        }
        self.wait().await;
        if self.is_cancelled() {
            return Some(self.end_with_cancel());
        }
        if let Some(item) = self.take_fault().await {
            return Some(item);
        }
        if self.done {
            return None;
        }
        let Some(event) = self.step() else {
            self.finish_stream();
            return None;
        };
        self.emitted += 1;
        Some(Ok(event))
    }

    /// Sleep between events, waking early when the request is cancelled.
    async fn wait(&self) {
        if self.event_delay.is_zero() {
            return;
        }
        match &self.cancel {
            Some(slot) => {
                tokio::select! {
                    _ = tokio::time::sleep(self.event_delay) => {}
                    _ = slot.notify.notified() => {}
                }
            }
            None => tokio::time::sleep(self.event_delay).await,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .map(|slot| slot.cancelled.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    fn end_with_cancel(&mut self) -> StreamItem {
        self.finish_stream();
        Ok(StreamEvent::error(StreamError::new(
            ErrorKind::Cancelled,
            "mock stream cancelled by the client",
        )))
    }

    fn finish_stream(&mut self) {
        self.done = true;
        self._registration = None;
    }

    /// Emit the configured failure once enough events were produced.
    ///
    /// `StreamInterrupted` ends the stream without a terminator; every other
    /// mode emits a terminal `Error` event.
    async fn take_fault(&mut self) -> Option<StreamItem> {
        let fault = self.mid_fault.clone()?;
        if self.fault_fired || self.emitted < fault.after_events {
            return None;
        }
        self.fault_fired = true;
        if fault.mode == FaultMode::Timeout && !fault.timeout.is_zero() {
            tokio::time::sleep(fault.timeout).await;
        }
        self.finish_stream();
        match fault.mode {
            FaultMode::StreamInterrupted => None,
            mode => Some(Ok(StreamEvent::error(StreamError::new(
                mode.error_kind(),
                fault_message(mode),
            )))),
        }
    }

    /// Produce the next protocol event, advancing the phase machine.
    ///
    /// Content is generated fragment by fragment from the configured reply;
    /// no complete response is built up front.
    fn step(&mut self) -> Option<StreamEvent> {
        loop {
            match self.phase {
                Phase::Start => {
                    self.phase = self.first_content_phase();
                    return Some(StreamEvent::start(
                        self.response_id.clone(),
                        self.request_id.clone(),
                        self.model.clone(),
                    ));
                }
                Phase::Reasoning => {
                    if let Some(chunk) =
                        take_chunk(&self.reasoning, &mut self.reasoning_pos, self.chunk_size)
                    {
                        return Some(StreamEvent::reasoning_delta(chunk));
                    }
                    self.phase = self.after_reasoning();
                }
                Phase::Text => {
                    if let Some(chunk) = take_chunk(&self.text, &mut self.text_pos, self.chunk_size)
                    {
                        return Some(StreamEvent::text_delta(chunk));
                    }
                    self.phase = self.after_text();
                }
                Phase::ToolStart(index) => {
                    if let Some(call) = self.tool_calls.get(index) {
                        let event = StreamEvent::ToolCallStart {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            index: Some(index as u32),
                        };
                        self.phase = Phase::ToolStart(index + 1);
                        return Some(event);
                    }
                    self.phase = Phase::Args;
                }
                Phase::Args => {
                    if self.args_done() {
                        self.phase = Phase::Usage;
                        continue;
                    }
                    if let Some(event) = self.next_arg_delta() {
                        return Some(event);
                    }
                    self.phase = Phase::Usage;
                }
                Phase::Usage => {
                    self.phase = Phase::Finish;
                    return Some(StreamEvent::usage(self.usage));
                }
                Phase::Finish => {
                    self.phase = Phase::Done;
                    self.finish_stream();
                    return Some(StreamEvent::finish(self.finish_reason.clone()));
                }
                Phase::Done => {
                    self.finish_stream();
                    return None;
                }
            }
        }
    }

    fn first_content_phase(&self) -> Phase {
        if !self.reasoning.is_empty() {
            Phase::Reasoning
        } else if !self.text.is_empty() {
            Phase::Text
        } else if !self.tool_calls.is_empty() {
            Phase::ToolStart(0)
        } else {
            Phase::Usage
        }
    }

    fn after_reasoning(&self) -> Phase {
        if !self.text.is_empty() {
            Phase::Text
        } else {
            self.after_text()
        }
    }

    fn after_text(&self) -> Phase {
        if self.tool_calls.is_empty() {
            Phase::Usage
        } else {
            Phase::ToolStart(0)
        }
    }

    fn args_done(&self) -> bool {
        self.tool_calls
            .iter()
            .zip(&self.arg_positions)
            .all(|(call, pos)| *pos >= call.fragments.len())
    }

    /// Round-robin argument fragments: each call contributes one fragment per
    /// round, so multiple calls interleave instead of finishing one at a time.
    fn next_arg_delta(&mut self) -> Option<StreamEvent> {
        let total = self.tool_calls.len();
        for _ in 0..total {
            let index = self.args_next_call % total;
            self.args_next_call = self.args_next_call.wrapping_add(1);
            let position = self.arg_positions[index];
            if let Some(delta) = self.tool_calls[index].fragments.get(position).cloned() {
                self.arg_positions[index] = position + 1;
                return Some(StreamEvent::ToolCallArgsDelta {
                    id: self.tool_calls[index].id.clone(),
                    delta,
                });
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Cancellation registry
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CancelSlot {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Default)]
struct CancelRegistry {
    slots: Mutex<HashMap<String, Arc<CancelSlot>>>,
}

impl CancelRegistry {
    fn register(&self, id: &str) -> Arc<CancelSlot> {
        let slot = Arc::new(CancelSlot::default());
        let _ = self.slots.lock().insert(id.to_string(), Arc::clone(&slot));
        slot
    }

    fn cancel(&self, id: &str) -> bool {
        let slot = self.slots.lock().get(id).cloned();
        match slot {
            Some(slot) => {
                slot.cancelled.store(true, Ordering::SeqCst);
                slot.notify.notify_one();
                true
            }
            None => false,
        }
    }

    fn remove(&self, id: &str, slot: &Arc<CancelSlot>) {
        let mut slots = self.slots.lock();
        if slots
            .get(id)
            .map(|current| Arc::ptr_eq(current, slot))
            .unwrap_or(false)
        {
            let _ = slots.remove(id);
        }
    }

    fn len(&self) -> usize {
        self.slots.lock().len()
    }
}

/// Removes the cancel registration when the stream is dropped or finished.
struct CancelRegistration {
    registry: Arc<CancelRegistry>,
    id: String,
    slot: Arc<CancelSlot>,
}

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        self.registry.remove(&self.id, &self.slot);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fault_message(mode: FaultMode) -> &'static str {
    match mode {
        FaultMode::AuthInvalid => "upstream rejected the credential",
        FaultMode::QuotaExhausted => "upstream reports exhausted quota",
        FaultMode::RateLimited => "upstream rate limited the request",
        FaultMode::UpstreamServer => "upstream returned a server error",
        FaultMode::Timeout => "upstream did not answer in time",
        FaultMode::Cancelled => "upstream cancelled the request",
        FaultMode::Transport => "connection to upstream failed",
        FaultMode::StreamInterrupted => "upstream stream was interrupted",
    }
}

fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

async fn sleep_for(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

fn take_chunk(text: &str, cursor: &mut usize, chunk_size: usize) -> Option<String> {
    let total = text.chars().count();
    if *cursor >= total {
        return None;
    }
    let end = (*cursor + chunk_size).min(total);
    let chunk = text.chars().skip(*cursor).take(end - *cursor).collect();
    *cursor = end;
    Some(chunk)
}

fn split_chunks(text: &str, chunk_size: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cursor = 0;
    while let Some(chunk) = take_chunk(text, &mut cursor, chunk_size) {
        chunks.push(chunk);
    }
    chunks
}
