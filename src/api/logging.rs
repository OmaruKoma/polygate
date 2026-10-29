//! Per-request logging shared by every API surface.
//!
//! One row per downstream request, written on completion (or when a stream
//! ends). Only metadata and counters are stored; messages, prompts and tool
//! arguments are deliberately absent.

use std::time::Instant;

use futures_util::StreamExt;

use crate::error::ErrorKind;
use crate::protocol::StreamEvent;
use crate::providers::ChatStream;
use crate::storage::{NewRequestLog, Storage};

/// Everything needed to write the log row, captured before the upstream call.
#[derive(Clone)]
pub struct LogContext {
    pub storage: Storage,
    pub request_id: String,
    pub provider: Option<String>,
    pub provider_kind: String,
    pub region: Option<String>,
    pub account_id: Option<String>,
    pub model_alias: String,
    pub upstream_model: String,
    pub api: &'static str,
    pub stream: bool,
    pub started: Instant,
}

impl LogContext {
    pub async fn write(self, status: u16, error: Option<ErrorKind>, input: i64, output: i64) {
        let latency_ms = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let event = NewRequestLog {
            request_id: self.request_id,
            provider: self.provider,
            provider_kind: self.provider_kind,
            region: self.region,
            account_id: self.account_id,
            model_alias: Some(self.model_alias),
            upstream_model: Some(self.upstream_model),
            api: self.api.to_string(),
            stream: self.stream,
            http_status: status,
            error_category: error.map(|kind| kind.category().as_str().to_string()),
            input_tokens: input,
            output_tokens: output,
            latency_ms,
        };
        if let Err(error) = self.storage.request_logs().record(&event).await {
            tracing::warn!(error = %error.message, "could not record request log");
        }
    }
}

/// Wrap a provider stream so the request log is written exactly once, when the
/// stream ends. Usage frames are captured; an error frame keeps its category.
pub fn observing_stream(inner: ChatStream, log: LogContext) -> ChatStream {
    struct State {
        inner: ChatStream,
        log: Option<LogContext>,
        input: i64,
        output: i64,
        error: Option<ErrorKind>,
    }
    let state = State {
        inner,
        log: Some(log),
        input: 0,
        output: 0,
        error: None,
    };
    Box::pin(futures_util::stream::unfold(
        state,
        |mut state| async move {
            match state.inner.next().await {
                Some(Ok(event)) => {
                    if let StreamEvent::Usage { usage } = &event {
                        state.input = usage.input_tokens as i64;
                        state.output = usage.output_tokens as i64;
                    }
                    if let StreamEvent::Error { error } = &event {
                        state.error = Some(error.kind);
                    }
                    Some((Ok(event), state))
                }
                Some(Err(error)) => {
                    state.error = Some(error.kind);
                    Some((Err(error), state))
                }
                None => {
                    if let Some(log) = state.log.take() {
                        let status = match state.error {
                            None => 200,
                            Some(kind) => kind.http_status().as_u16(),
                        };
                        log.write(status, state.error, state.input, state.output)
                            .await;
                    }
                    None
                }
            }
        },
    ))
}
