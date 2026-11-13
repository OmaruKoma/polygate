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
use crate::storage::{NewRequestLog, NewUsageEvent, Storage};

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

    /// Usage accounting for a stream, which has no response object to read.
    /// A failed stream is already covered by the request log, so it is not
    /// counted as usage.
    pub async fn record_usage(&self, input: i64, output: i64) {
        let event = NewUsageEvent {
            request_id: uuid::Uuid::new_v4().to_string(),
            provider_kind: self.provider_kind.clone(),
            account_id: self.account_id.clone(),
            model: Some(self.model_alias.clone()),
            input_tokens: input,
            output_tokens: output,
            status: "ok".to_string(),
        };
        if let Err(error) = self.storage.usage().record(&event).await {
            tracing::warn!(error = %error.message, "could not record usage");
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
                        if state.error.is_none() {
                            log.record_usage(state.input, state.output).await;
                        }
                        log.write(status, state.error, state.input, state.output)
                            .await;
                    }
                    None
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Usage;
    use std::time::Instant;

    fn context(storage: Storage) -> LogContext {
        LogContext {
            storage,
            request_id: "req-1".into(),
            provider: Some("mock-cn".into()),
            provider_kind: "mock".into(),
            region: Some("cn".into()),
            account_id: None,
            model_alias: "alias".into(),
            upstream_model: "upstream".into(),
            api: "chat",
            stream: true,
            started: Instant::now(),
        }
    }

    #[tokio::test]
    async fn a_finished_stream_counts_as_usage() {
        let storage = Storage::open_memory().await.unwrap();
        let events = vec![Ok(StreamEvent::Usage {
            usage: Usage::new(120, 34),
        })];
        let inner: ChatStream = Box::pin(futures_util::stream::iter(events));
        let mut stream = observing_stream(inner, context(storage.clone()));
        while stream.next().await.is_some() {}

        let summary = storage.usage().summary(None).await.unwrap();
        assert_eq!(
            (
                summary.requests,
                summary.input_tokens,
                summary.output_tokens
            ),
            (1, 120, 34)
        );
    }

    #[tokio::test]
    async fn a_failed_stream_is_logged_but_not_counted_as_usage() {
        let storage = Storage::open_memory().await.unwrap();
        let events = vec![Ok(StreamEvent::Error {
            error: crate::protocol::StreamError::new(ErrorKind::RateLimited, "slow down"),
        })];
        let inner: ChatStream = Box::pin(futures_util::stream::iter(events));
        let mut stream = observing_stream(inner, context(storage.clone()));
        while stream.next().await.is_some() {}

        assert_eq!(storage.usage().summary(None).await.unwrap().requests, 0);
        assert_eq!(
            storage.request_logs().list_recent(5).await.unwrap().len(),
            1
        );
    }
}
