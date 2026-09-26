//! Test helpers: a scripted model provider and a few mock tools and hooks.
//!
//! These helpers are always compiled so that downstream crates (the pipeline,
//! plugins, the CLI) can exercise the agent runtime without a network
//! connection. They are small and have no side effects beyond what their
//! documentation states.
//!
//! ```
//! use std::sync::Arc;
//! use vibe_agents::test_support::{ScriptedProvider, text_response};
//!
//! let provider = Arc::new(ScriptedProvider::new(vec![text_response("hello")]));
//! assert_eq!(provider.remaining(), 1);
//! ```

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vibe_core::plugin::Hook;
use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, ContentBlock, Error, HookDecision, Message,
    ModelProvider, Result, Role, StopReason, Tool, ToolContext, ToolOutput, Usage,
};

/// Build an assistant response made of a single text block that ends the turn.
#[must_use]
pub fn text_response(text: impl Into<String>) -> CompletionResponse {
    CompletionResponse {
        message: Message::assistant(text),
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        model: "scripted".into(),
    }
}

/// Build an assistant response that requests the given tool calls.
///
/// Each call is `(id, tool_name, input)`.
#[must_use]
pub fn tool_use_response(calls: Vec<(&str, &str, serde_json::Value)>) -> CompletionResponse {
    let content = calls
        .into_iter()
        .map(|(id, name, input)| ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        })
        .collect();
    CompletionResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        model: "scripted".into(),
    }
}

/// Build a response with a custom stop reason and a single text block.
#[must_use]
pub fn response_with_stop(text: impl Into<String>, stop_reason: StopReason) -> CompletionResponse {
    CompletionResponse {
        stop_reason,
        ..text_response(text)
    }
}

/// A [`ModelProvider`] that replays a queue of canned responses and records
/// every request it receives.
///
/// When the queue is exhausted, `complete` fails with an
/// [`vibe_core::ErrorKind::Other`] error so that a test never hangs.
#[derive(Default)]
pub struct ScriptedProvider {
    script: Mutex<VecDeque<Result<CompletionResponse>>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl std::fmt::Debug for ScriptedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedProvider")
            .field("remaining", &self.remaining())
            .field("requests", &self.request_count())
            .finish()
    }
}

impl ScriptedProvider {
    /// Provider that answers with `responses`, in order.
    #[must_use]
    pub fn new(responses: Vec<CompletionResponse>) -> Self {
        Self {
            script: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Provider whose script may contain errors as well as responses.
    #[must_use]
    pub fn with_results(results: Vec<Result<CompletionResponse>>) -> Self {
        Self {
            script: Mutex::new(results.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Append a response to the end of the script.
    pub fn push(&self, response: CompletionResponse) {
        lock(&self.script).push_back(Ok(response));
    }

    /// Append an error to the end of the script.
    pub fn push_error(&self, error: Error) {
        lock(&self.script).push_back(Err(error));
    }

    /// Number of scripted answers not consumed yet.
    #[must_use]
    pub fn remaining(&self) -> usize {
        lock(&self.script).len()
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<CompletionRequest> {
        lock(&self.requests).clone()
    }

    /// Number of requests received so far.
    #[must_use]
    pub fn request_count(&self) -> usize {
        lock(&self.requests).len()
    }
}

/// Lock a mutex, recovering from poisoning (a panicking test must not cascade).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "scripted".into(),
            supports_tools: true,
            supports_thinking: true,
            default_model: "scripted".into(),
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        lock(&self.requests).push(request);
        let next = lock(&self.script).pop_front();
        next.unwrap_or_else(|| Err(Error::other("scripted provider: script exhausted")))
    }
}

/// Tracks how many tool calls are in flight at once, and the order in which
/// calls started.
#[derive(Debug, Default)]
pub struct ConcurrencyProbe {
    current: AtomicUsize,
    max: AtomicUsize,
    order: Mutex<Vec<String>>,
}

impl ConcurrencyProbe {
    /// New shared probe.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Highest number of simultaneous calls observed.
    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.max.load(Ordering::SeqCst)
    }

    /// Labels of the calls, in the order they started.
    #[must_use]
    pub fn order(&self) -> Vec<String> {
        lock(&self.order).clone()
    }

    fn enter(&self, label: String) {
        lock(&self.order).push(label);
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.current.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A read-only tool that returns its `text` input (or the whole input as
/// JSON). It can optionally sleep and report to a [`ConcurrencyProbe`].
#[derive(Debug, Clone)]
pub struct EchoTool {
    name: String,
    delay: Duration,
    probe: Option<Arc<ConcurrencyProbe>>,
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoTool {
    /// Tool named `echo` with no delay.
    #[must_use]
    pub fn new() -> Self {
        Self {
            name: "echo".into(),
            delay: Duration::ZERO,
            probe: None,
        }
    }

    /// Rename the tool.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sleep this long on every call.
    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Report calls to `probe`.
    #[must_use]
    pub fn with_probe(mut self, probe: Arc<ConcurrencyProbe>) -> Self {
        self.probe = Some(probe);
        self
    }
}

fn label_of(input: &serde_json::Value) -> String {
    input
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| input.to_string(), str::to_string)
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Echo the `text` argument back."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
        })
    }

    async fn call(&self, _ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        let label = label_of(&input);
        if let Some(p) = &self.probe {
            p.enter(label.clone());
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if let Some(p) = &self.probe {
            p.leave();
        }
        Ok(ToolOutput::ok(label))
    }
}

/// How a [`FailingTool`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureMode {
    /// Return `Err(Error::tool(..))`.
    Error,
    /// Panic inside `call`.
    Panic,
}

/// A tool that always fails, either with an error or a panic.
#[derive(Debug, Clone)]
pub struct FailingTool {
    name: String,
    mode: FailureMode,
}

impl FailingTool {
    /// Tool named `fail` that returns an error.
    #[must_use]
    pub fn error() -> Self {
        Self {
            name: "fail".into(),
            mode: FailureMode::Error,
        }
    }

    /// Tool named `panic` that panics.
    #[must_use]
    pub fn panicking() -> Self {
        Self {
            name: "panic".into(),
            mode: FailureMode::Panic,
        }
    }
}

#[async_trait::async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Always fails."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _ctx: &ToolContext, _input: serde_json::Value) -> Result<ToolOutput> {
        match self.mode {
            FailureMode::Error => Err(Error::tool("boom: the tool failed on purpose")),
            FailureMode::Panic => panic!("the tool panicked on purpose"),
        }
    }
}

/// A mutating tool that sleeps and reports to a [`ConcurrencyProbe`].
///
/// Returns `wrote <text>`.
#[derive(Debug, Clone)]
pub struct SlowMutatingTool {
    name: String,
    delay: Duration,
    probe: Arc<ConcurrencyProbe>,
}

impl SlowMutatingTool {
    /// Tool named `mutate` sleeping `delay` per call.
    #[must_use]
    pub fn new(delay: Duration, probe: Arc<ConcurrencyProbe>) -> Self {
        Self {
            name: "mutate".into(),
            delay,
            probe,
        }
    }
}

#[async_trait::async_trait]
impl Tool for SlowMutatingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Pretend to modify the workspace, slowly."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn call(&self, _ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        let label = label_of(&input);
        self.probe.enter(label.clone());
        tokio::time::sleep(self.delay).await;
        self.probe.leave();
        Ok(ToolOutput::ok(format!("wrote {label}")))
    }
}

/// A tool that returns a string of `len` repeated `x` characters.
#[derive(Debug, Clone)]
pub struct BigOutputTool {
    len: usize,
}

impl BigOutputTool {
    /// Tool named `big` returning `len` characters.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self { len }
    }
}

#[async_trait::async_trait]
impl Tool for BigOutputTool {
    fn name(&self) -> &str {
        "big"
    }

    fn description(&self) -> &str {
        "Return a large output."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(&self, _ctx: &ToolContext, _input: serde_json::Value) -> Result<ToolOutput> {
        Ok(ToolOutput::ok("x".repeat(self.len)))
    }
}

/// A hook that vetoes every call to one tool and counts `after_tool` calls.
#[derive(Debug)]
pub struct VetoHook {
    tool: String,
    reason: String,
    after_calls: AtomicUsize,
}

impl VetoHook {
    /// Veto calls to `tool` with `reason`.
    #[must_use]
    pub fn new(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            reason: reason.into(),
            after_calls: AtomicUsize::new(0),
        }
    }

    /// Number of `after_tool` notifications received.
    #[must_use]
    pub fn after_calls(&self) -> usize {
        self.after_calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Hook for VetoHook {
    fn name(&self) -> &str {
        "veto"
    }

    async fn before_tool(
        &self,
        _ctx: &ToolContext,
        tool: &str,
        _input: &serde_json::Value,
    ) -> HookDecision {
        if tool == self.tool {
            HookDecision::Abort(self.reason.clone())
        } else {
            HookDecision::Continue
        }
    }

    async fn after_tool(&self, _ctx: &ToolContext, _tool: &str, _output: &ToolOutput) {
        self.after_calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// A hook that appends a fixed text to every system prompt.
#[derive(Debug, Clone)]
pub struct PromptHook(pub String);

#[async_trait::async_trait]
impl Hook for PromptHook {
    fn name(&self) -> &str {
        "prompt"
    }

    async fn augment_prompt(
        &self,
        _role: &vibe_core::AgentRole,
        _task: &vibe_core::Task,
    ) -> Option<String> {
        Some(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_provider_replays_and_records() {
        let p = ScriptedProvider::new(vec![text_response("a")]);
        let r = p
            .complete(CompletionRequest::new("m", vec![Message::user("hi")]))
            .await
            .unwrap();
        assert_eq!(r.message.text(), "a");
        assert_eq!(p.request_count(), 1);
        assert!(
            p.complete(CompletionRequest::new("m", vec![]))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failing_tool_errors() {
        let t = FailingTool::error();
        assert!(
            t.call(&ToolContext::new("."), serde_json::json!({}))
                .await
                .is_err()
        );
    }
}
