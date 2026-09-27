//! A scripted, in-memory [`ModelProvider`] for tests and dry runs.
//!
//! ```
//! use serde_json::json;
//! use vibe_core::{CompletionRequest, ErrorKind, Message, ModelProvider, StopReason};
//! use vibe_providers::MockProvider;
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let mock = MockProvider::new()
//!     .reply_tool_use("read_file", json!({"path": "src/lib.rs"}))
//!     .reply_error(ErrorKind::RateLimited)
//!     .reply_text("All done.");
//!
//! let req = CompletionRequest::new("mock-model", vec![Message::user("hi")]);
//! let first = mock.complete(req.clone()).await.unwrap();
//! assert_eq!(first.stop_reason, StopReason::ToolUse);
//! assert_eq!(mock.complete(req.clone()).await.unwrap_err().kind, ErrorKind::RateLimited);
//! assert_eq!(mock.complete(req).await.unwrap().message.text(), "All done.");
//! assert_eq!(mock.requests().len(), 3);
//! # });
//! ```

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, ContentBlock, DeltaSink, Error, ErrorKind, Message,
    ModelProvider, Result, Role, StopReason, StreamDelta, Usage,
};

/// Closure producing a response for a request once the script is exhausted.
pub type MockHandler = Box<dyn Fn(&CompletionRequest) -> Result<CompletionResponse> + Send + Sync>;

/// One scripted step.
#[derive(Debug, Clone)]
pub enum MockStep {
    /// Return this response (the `model` field is filled from the request when
    /// empty, and usage is estimated when zero).
    Respond(CompletionResponse),
    /// Fail with an error of this kind.
    Fail {
        /// Error kind.
        kind: ErrorKind,
        /// Error message.
        message: String,
        /// Optional retry hint.
        retry_after: Option<Duration>,
    },
}

/// Scripted provider: replays queued steps in order and records every request.
///
/// Once the script is empty, the optional handler set with
/// [`MockProvider::with_handler`] answers; without one, calls fail with
/// [`ErrorKind::Other`] ("mock script exhausted").
pub struct MockProvider {
    name: String,
    default_model: String,
    supports_thinking: bool,
    script: Mutex<VecDeque<MockStep>>,
    requests: Mutex<Vec<CompletionRequest>>,
    handler: Option<MockHandler>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for MockProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockProvider")
            .field("name", &self.name)
            .field("default_model", &self.default_model)
            .field("remaining", &self.remaining())
            .field("recorded", &self.request_count())
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockProvider {
    /// Empty script, name `mock`, default model `mock-model`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            name: "mock".into(),
            default_model: "mock-model".into(),
            supports_thinking: true,
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            handler: None,
            next_id: AtomicU64::new(1),
        }
    }

    /// Set the name reported by [`ModelProvider::info`].
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the default model reported by [`ModelProvider::info`].
    #[must_use]
    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Set whether the provider claims extended-thinking support.
    #[must_use]
    pub fn with_thinking_support(mut self, supported: bool) -> Self {
        self.supports_thinking = supported;
        self
    }

    /// Answer with `handler` whenever the script is empty.
    #[must_use]
    pub fn with_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&CompletionRequest) -> Result<CompletionResponse> + Send + Sync + 'static,
    {
        self.handler = Some(Box::new(handler));
        self
    }

    /// Queue an arbitrary step.
    #[must_use]
    pub fn then(self, step: MockStep) -> Self {
        self.push(step);
        self
    }

    /// Queue a full response.
    #[must_use]
    pub fn reply(self, response: CompletionResponse) -> Self {
        self.then(MockStep::Respond(response))
    }

    /// Queue an assistant text answer ending the turn.
    #[must_use]
    pub fn reply_text(self, text: impl Into<String>) -> Self {
        self.reply(text_response(text))
    }

    /// Queue a single tool call. Call ids are generated (`mock_call_<n>`).
    #[must_use]
    pub fn reply_tool_use(self, name: impl Into<String>, input: serde_json::Value) -> Self {
        let block = self.tool_use_block(name.into(), input);
        self.reply(blocks_response(vec![block], StopReason::ToolUse))
    }

    /// Queue several tool calls in one assistant message.
    #[must_use]
    pub fn reply_tool_uses<I, S>(self, calls: I) -> Self
    where
        I: IntoIterator<Item = (S, serde_json::Value)>,
        S: Into<String>,
    {
        let blocks = calls
            .into_iter()
            .map(|(name, input)| self.tool_use_block(name.into(), input))
            .collect();
        self.reply(blocks_response(blocks, StopReason::ToolUse))
    }

    /// Queue a failure of the given kind with a generic message.
    #[must_use]
    pub fn reply_error(self, kind: ErrorKind) -> Self {
        self.reply_error_message(kind, format!("simulated {kind:?} error"))
    }

    /// Queue a failure with a custom message.
    #[must_use]
    pub fn reply_error_message(self, kind: ErrorKind, message: impl Into<String>) -> Self {
        self.then(MockStep::Fail {
            kind,
            message: message.into(),
            retry_after: None,
        })
    }

    /// Queue a step through a shared reference (e.g. behind an `Arc`).
    pub fn push(&self, step: MockStep) {
        lock(&self.script).push_back(step);
    }

    /// Queue a text answer through a shared reference.
    pub fn push_text(&self, text: impl Into<String>) {
        self.push(MockStep::Respond(text_response(text)));
    }

    /// Queue a failure through a shared reference.
    pub fn push_error(&self, kind: ErrorKind) {
        self.push(MockStep::Fail {
            kind,
            message: format!("simulated {kind:?} error"),
            retry_after: None,
        });
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<CompletionRequest> {
        lock(&self.requests).clone()
    }

    /// The most recent request, if any.
    #[must_use]
    pub fn last_request(&self) -> Option<CompletionRequest> {
        lock(&self.requests).last().cloned()
    }

    /// Number of requests received.
    #[must_use]
    pub fn request_count(&self) -> usize {
        lock(&self.requests).len()
    }

    /// Number of scripted steps not consumed yet.
    #[must_use]
    pub fn remaining(&self) -> usize {
        lock(&self.script).len()
    }

    fn tool_use_block(&self, name: String, input: serde_json::Value) -> ContentBlock {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        ContentBlock::ToolUse {
            id: format!("mock_call_{n}"),
            name,
            input,
        }
    }
}

/// A response with a single assistant text block and `EndTurn`.
#[must_use]
pub fn text_response(text: impl Into<String>) -> CompletionResponse {
    blocks_response(
        vec![ContentBlock::Text { text: text.into() }],
        StopReason::EndTurn,
    )
}

fn blocks_response(content: Vec<ContentBlock>, stop_reason: StopReason) -> CompletionResponse {
    CompletionResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage: Usage::default(),
        model: String::new(),
    }
}

/// Lock a mutex, recovering the data if a panicking test poisoned it.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            supports_tools: true,
            supports_thinking: self.supports_thinking,
            default_model: self.default_model.clone(),
        }
    }

    /// Streams the text of the scripted answer word by word (thinking
    /// first), then returns the answer unchanged.
    async fn complete_streaming(
        &self,
        request: CompletionRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<CompletionResponse> {
        let response = self.complete(request).await?;
        for block in &response.message.content {
            let (text, thinking) = match block {
                ContentBlock::Text { text } => (text, false),
                ContentBlock::Thinking { text } => (text, true),
                _ => continue,
            };
            for piece in text.split_inclusive(' ') {
                let text = piece.to_string();
                on_delta(if thinking {
                    StreamDelta::Thinking { text }
                } else {
                    StreamDelta::Text { text }
                });
            }
        }
        Ok(response)
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        lock(&self.requests).push(request.clone());
        let step = lock(&self.script).pop_front();
        let mut response = match step {
            Some(MockStep::Respond(response)) => response,
            Some(MockStep::Fail {
                kind,
                message,
                retry_after,
            }) => {
                let err = Error::new(kind, message);
                return Err(match retry_after {
                    Some(after) => err.with_retry_after(after),
                    None => err,
                });
            }
            None => match &self.handler {
                Some(handler) => handler(&request)?,
                None => {
                    return Err(Error::other(format!(
                        "mock script exhausted after {} request(s)",
                        self.request_count()
                    )));
                }
            },
        };
        if response.model.is_empty() {
            response.model = if request.model.is_empty() {
                self.default_model.clone()
            } else {
                request.model.clone()
            };
        }
        if response.usage == Usage::default() {
            response.usage = Usage {
                input_tokens: request.estimate_tokens() as u64,
                output_tokens: response.message.estimate_tokens() as u64,
                ..Usage::default()
            };
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn req() -> CompletionRequest {
        CompletionRequest::new("m", vec![Message::user("hello there")])
    }

    #[tokio::test]
    async fn replays_script_in_order_and_records() {
        let mock = MockProvider::new()
            .reply_text("one")
            .reply_tool_uses([("a", json!({})), ("b", json!({"x": 1}))]);
        let r1 = mock.complete(req()).await.unwrap();
        assert_eq!(r1.message.text(), "one");
        assert_eq!(r1.model, "m");
        assert!(r1.usage.input_tokens > 0);
        let r2 = mock.complete(req()).await.unwrap();
        let calls: Vec<_> = r2
            .message
            .tool_uses()
            .map(|(id, n, _)| (id.to_string(), n.to_string()))
            .collect();
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].0, calls[1].0);
        assert_eq!(calls[1].1, "b");
        assert_eq!(mock.request_count(), 2);
        assert_eq!(mock.remaining(), 0);
        assert_eq!(mock.last_request().unwrap().model, "m");
    }

    #[tokio::test]
    async fn exhausted_script_errors_without_handler() {
        let mock = MockProvider::new();
        let err = mock.complete(req()).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Other);
        assert!(err.message.contains("exhausted"));
    }

    #[tokio::test]
    async fn handler_answers_when_script_empty() {
        let mock = MockProvider::new()
            .reply_text("scripted")
            .with_handler(|r| Ok(text_response(format!("echo {}", r.messages.len()))));
        assert_eq!(
            mock.complete(req()).await.unwrap().message.text(),
            "scripted"
        );
        assert_eq!(mock.complete(req()).await.unwrap().message.text(), "echo 1");
    }

    #[tokio::test]
    async fn errors_and_shared_push() {
        let mock = Arc::new(MockProvider::new().with_name("m1").with_default_model("dm"));
        mock.push_error(ErrorKind::AuthFailed);
        mock.push(MockStep::Fail {
            kind: ErrorKind::RateLimited,
            message: "slow".into(),
            retry_after: Some(Duration::from_secs(2)),
        });
        mock.push_text("ok");
        assert_eq!(
            mock.complete(req()).await.unwrap_err().kind,
            ErrorKind::AuthFailed
        );
        let e = mock.complete(req()).await.unwrap_err();
        assert_eq!(e.retry_after, Some(Duration::from_secs(2)));
        let mut r = req();
        r.model.clear();
        assert_eq!(mock.complete(r).await.unwrap().model, "dm");
        assert_eq!(mock.info().name, "m1");
    }
}
