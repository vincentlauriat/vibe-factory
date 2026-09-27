//! The agent runtime: an agentic tool-use loop around a [`ModelProvider`].
//!
//! See the crate documentation for a diagram of the loop.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::join_all;
use tokio::sync::watch;
use vibe_core::agent::ThinkingLevel;
use vibe_core::{
    AgentOutcome, AgentRole, AgentSpec, AgentStop, CompletionRequest, CompletionResponse,
    ContentBlock, Error, ErrorKind, Event, EventBus, HookDecision, Message, ModelProvider,
    Permissions, PromptTemplate, Registry, Result, Role, RunBudget, RunId, StopReason, SubtaskId,
    Task, ToolContext, ToolOutput, ToolRegistry, Usage,
};

use crate::prompts::strip_doc_comment;

/// Default context window, in tokens.
pub const DEFAULT_CONTEXT_WINDOW: usize = 200_000;
/// Default maximum number of characters of a tool output shown to the model.
pub const DEFAULT_MAX_TOOL_OUTPUT_CHARS: usize = 100_000;
/// Share of the context window (percent) at which the "finish now" warning is
/// injected.
pub const CONTEXT_WARNING_PERCENT: usize = 85;
/// Share of the context window (percent) at which the run stops with
/// [`AgentStop::ContextWindow`].
pub const CONTEXT_STOP_PERCENT: usize = 90;
/// Share of `max_steps` (percent) at which converging agents are told to
/// produce their final answer.
pub const CONVERGE_STEP_PERCENT: u32 = 75;

/// Message injected once when the conversation nears the context limit.
pub const CONTEXT_WARNING_MESSAGE: &str =
    "You are near the context limit: finish now, write your final output.";
/// Message injected once for converging agents at 75% of their step budget.
pub const CONVERGE_MESSAGE: &str = "Converge now: produce your final structured answer.";
/// Message sent when the model ran out of output tokens mid-answer (a
/// second cut in a row stops the run with [`TRUNCATED_TWICE_MESSAGE`]).
pub const CONTINUE_NUDGE: &str = "continue (your previous answer was cut off by the output limit; resume exactly where it stopped)";

/// Number of characters of a tool output kept in [`Event::ToolReturned`].
const PREVIEW_CHARS: usize = 200;

/// Upper bound of the wait before retrying a provider call, whatever the
/// provider's `retry_after` hint says.
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Stop message used when the model is cut by the output limit twice in a
/// row.
pub const TRUNCATED_TWICE_MESSAGE: &str = "output truncated twice";

/// Key under which providers store tool arguments they could not parse as
/// JSON (`{"_raw": "<original text>"}`). A call carrying it is never executed.
pub const RAW_ARGUMENTS_KEY: &str = "_raw";

/// Error returned to the model for a call whose arguments were not valid JSON.
pub const INVALID_ARGUMENTS_MESSAGE: &str = "the arguments of this call were not valid JSON \
(possibly truncated); repeat the call with complete JSON arguments";

/// Runs an [`AgentSpec`] against a [`ModelProvider`] with a set of tools.
///
/// The runner is cheap to clone and can be reused for any number of runs;
/// every call to [`AgentRunner::run`] starts a fresh conversation.
///
/// ```
/// use std::sync::Arc;
/// use vibe_agents::AgentRunner;
/// use vibe_agents::test_support::{ScriptedProvider, text_response};
/// use vibe_core::{AgentRole, AgentSpec, EventBus, Registry, ToolRegistry};
///
/// # tokio_test_block(async {
/// let provider = Arc::new(ScriptedProvider::new(vec![text_response("done")]));
/// let runner = AgentRunner::new(
///     provider,
///     "scripted".into(),
///     ToolRegistry::new(),
///     Arc::new(Registry::new()),
///     EventBus::default(),
/// );
/// let spec = AgentSpec::new(AgentRole::Custom("demo".into()), "You are helpful.");
/// let outcome = runner.run(&spec, "Say done".into()).await.unwrap();
/// assert_eq!(outcome.final_text, "done");
/// # });
/// # fn tokio_test_block<F: std::future::Future>(f: F) -> F::Output {
/// #     tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
/// # }
/// ```
#[derive(Clone)]
pub struct AgentRunner {
    provider: Arc<dyn ModelProvider>,
    model: String,
    tools: ToolRegistry,
    registry: Arc<Registry>,
    events: EventBus,
    context_window: usize,
    run_id: RunId,
    task: Option<Task>,
    subtask: Option<SubtaskId>,
    tool_context: ToolContext,
    max_tool_output_chars: usize,
    extra_vars: BTreeMap<String, String>,
    cancel: Option<watch::Receiver<bool>>,
    budget: Option<Arc<RunBudget>>,
    max_provider_retries: u32,
    retry_base_delay: Duration,
}

impl std::fmt::Debug for AgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRunner")
            .field("provider", &self.provider.info().name)
            .field("model", &self.model)
            .field("tools", &self.tools)
            .field("context_window", &self.context_window)
            .field("run_id", &self.run_id)
            .field("workspace_root", &self.tool_context.workspace_root)
            .finish_non_exhaustive()
    }
}

/// One tool call requested by the model.
#[derive(Debug, Clone)]
struct ToolCall {
    id: String,
    name: String,
    input: serde_json::Value,
}

impl AgentRunner {
    /// Create a runner.
    ///
    /// * `provider` / `model`: where completions come from.
    /// * `tools`: every tool the runner may offer; each [`AgentSpec`] narrows
    ///   this set with its [`vibe_core::ToolSelection`].
    /// * `registry`: used for hooks (`before_tool`, `after_tool`,
    ///   `augment_prompt`).
    /// * `events`: bus on which progress is published.
    ///
    /// The tool context defaults to the current directory with
    /// [`Permissions::default`].
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: String,
        tools: ToolRegistry,
        registry: Arc<Registry>,
        events: EventBus,
    ) -> Self {
        Self {
            provider,
            model,
            tools,
            registry,
            events,
            context_window: DEFAULT_CONTEXT_WINDOW,
            run_id: RunId::new(),
            task: None,
            subtask: None,
            tool_context: ToolContext::new("."),
            max_tool_output_chars: DEFAULT_MAX_TOOL_OUTPUT_CHARS,
            extra_vars: BTreeMap::new(),
            cancel: None,
            budget: None,
            max_provider_retries: 2,
            retry_base_delay: Duration::from_secs(1),
        }
    }

    /// Size of the model context window in tokens (default `200_000`).
    #[must_use]
    pub fn context_window(mut self, tokens: usize) -> Self {
        self.context_window = tokens.max(1);
        self
    }

    /// Run id attached to published events (default: a fresh id).
    #[must_use]
    pub fn run_id(mut self, run_id: RunId) -> Self {
        self.run_id = run_id;
        self
    }

    /// Task being worked on. Provides the `task_title` and
    /// `task_description` prompt variables and is passed to hooks.
    #[must_use]
    pub fn task(mut self, task: Task) -> Self {
        self.task = Some(task);
        self
    }

    /// Subtask being worked on, reported in [`Event::AgentStarted`].
    #[must_use]
    pub fn subtask(mut self, subtask: SubtaskId) -> Self {
        self.subtask = Some(subtask);
        self
    }

    /// Base context handed to every tool call. The runner overrides its
    /// `agent` field with the role name and fills `task_id` from
    /// [`AgentRunner::task`] when set.
    #[must_use]
    pub fn tool_context(mut self, ctx: ToolContext) -> Self {
        self.tool_context = ctx;
        self
    }

    /// Permissions granted to tool calls (applied to the current tool
    /// context; call after [`AgentRunner::tool_context`]).
    #[must_use]
    pub fn permissions(mut self, permissions: Permissions) -> Self {
        self.tool_context.permissions = permissions;
        self
    }

    /// Maximum number of characters of a tool output sent to the model
    /// (default `100_000`). Longer outputs are truncated and the full text is
    /// saved under `.vibe/tool-output/` in the workspace.
    #[must_use]
    pub fn max_tool_output_chars(mut self, chars: usize) -> Self {
        self.max_tool_output_chars = chars.max(1);
        self
    }

    /// Add a prompt variable. Extra variables override the built-in ones
    /// (`task_title`, `task_description`, `workspace_root`, `date`).
    #[must_use]
    pub fn var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_vars.insert(name.into(), value.into());
        self
    }

    /// Add several prompt variables at once (see [`AgentRunner::var`]).
    #[must_use]
    pub fn extra_vars(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (k, v) in vars {
            self.extra_vars.insert(k.into(), v.into());
        }
        self
    }

    /// Cancellation token: when the watched value becomes `true`, the run
    /// stops before its next step with [`AgentStop::Cancelled`].
    #[must_use]
    pub fn cancel_token(mut self, cancel: watch::Receiver<bool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Run budget: the usage of every model call is added to it, and once
    /// one of its limits is reached the run stops before its next step with
    /// [`AgentStop::Cancelled`], like a cancellation. Callers tell the two
    /// apart with [`RunBudget::exceeded`].
    #[must_use]
    pub fn budget(mut self, budget: Arc<RunBudget>) -> Self {
        self.budget = Some(budget);
        self
    }

    /// How many times a retryable provider error (rate limit, server error,
    /// network) is retried by the runner before the run stops with
    /// [`AgentStop::Error`] (default 2). Providers are expected to retry on
    /// their own; this is a last line of defence.
    #[must_use]
    pub fn max_provider_retries(mut self, retries: u32) -> Self {
        self.max_provider_retries = retries;
        self
    }

    /// Base delay of the runner's exponential backoff (default 1s). The
    /// provider's `retry_after` hint takes precedence when present.
    #[must_use]
    pub fn retry_base_delay(mut self, delay: Duration) -> Self {
        self.retry_base_delay = delay;
        self
    }

    /// The provider used by this runner.
    #[must_use]
    pub fn provider(&self) -> &Arc<dyn ModelProvider> {
        &self.provider
    }

    /// The model id used by this runner.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The event bus used by this runner.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// The run id attached to events.
    #[must_use]
    pub fn current_run_id(&self) -> RunId {
        self.run_id
    }

    /// The workspace root tools are confined to.
    #[must_use]
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.tool_context.workspace_root
    }

    /// Run `spec` on a fresh conversation that starts with `user_message`.
    ///
    /// Provider failures do not produce an `Err`: they end the run with
    /// [`AgentStop::Error`] (or [`AgentStop::ContextWindow`] for
    /// [`ErrorKind::ContextTooLong`]) so that the transcript is never lost.
    pub async fn run(&self, spec: &AgentSpec, user_message: String) -> Result<AgentOutcome> {
        self.run_conversation(spec, vec![Message::user(user_message)])
            .await
    }

    /// Continue an existing transcript (typically `outcome.messages` of an
    /// earlier run) with a new user message.
    pub async fn resume(
        &self,
        spec: &AgentSpec,
        history: Vec<Message>,
        user_message: String,
    ) -> Result<AgentOutcome> {
        let mut messages = history;
        // A dangling tool call without results would be rejected by providers.
        if let Some(last) = messages.last()
            && last.role == Role::Assistant
            && last.has_tool_use()
        {
            let results = last
                .tool_uses()
                .map(|(id, _, _)| ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: "Tool call was not executed.".into(),
                    is_error: true,
                })
                .collect();
            messages.push(Message::tool_results(results));
        }
        push_user_text(&mut messages, user_message);
        self.run_conversation(spec, messages).await
    }

    /// Render the system prompt of `spec`: its template with the prompt
    /// variables substituted, followed by every hook augmentation.
    pub async fn system_prompt(&self, spec: &AgentSpec) -> String {
        let mut vars = BTreeMap::new();
        let (title, description) = self
            .task
            .as_ref()
            .map(|t| (t.title.clone(), t.description.clone()))
            .unwrap_or_default();
        vars.insert("task_title".to_string(), title);
        vars.insert("task_description".to_string(), description);
        vars.insert(
            "workspace_root".to_string(),
            self.tool_context.workspace_root.display().to_string(),
        );
        vars.insert(
            "date".to_string(),
            chrono::Utc::now().format("%Y-%m-%d").to_string(),
        );
        vars.extend(self.extra_vars.clone());
        let template = PromptTemplate::new(strip_doc_comment(&spec.system_prompt));
        let mut out = template.render(&vars).trim().to_string();

        // Hooks need a task; use a blank placeholder when none is set.
        let placeholder;
        let task = match &self.task {
            Some(t) => t,
            None => {
                placeholder = Task::new("", "");
                &placeholder
            }
        };
        for extra in self.registry.augment_prompt(&spec.role, task).await {
            let extra = extra.trim();
            if !extra.is_empty() {
                out.push_str("\n\n");
                out.push_str(extra);
            }
        }
        out
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|rx| *rx.borrow())
            || self.budget.as_ref().is_some_and(|b| b.exceeded().is_some())
    }

    async fn run_conversation(
        &self,
        spec: &AgentSpec,
        mut messages: Vec<Message>,
    ) -> Result<AgentOutcome> {
        let role = spec.role.clone();
        let system = self.system_prompt(spec).await;
        let selected = spec.tools.resolve(self.tools.names());
        let tools = self.tools.subset(selected.iter().map(String::as_str));
        let tool_specs = tools.specs();
        let tools_tokens = serde_json::to_string(&tool_specs)
            .map(|s| s.len().div_ceil(4))
            .unwrap_or(0);

        let mut ctx = self.tool_context.clone();
        ctx.agent = role.name();
        if let Some(task) = &self.task {
            ctx.task_id = Some(task.id);
        }

        let thinking_budget = spec.thinking.budget();
        let max_tokens = effective_max_tokens(spec.max_tokens, spec.thinking);
        let converge_at = needs_convergence(spec).then(|| {
            (spec.max_steps * CONVERGE_STEP_PERCENT)
                .div_ceil(100)
                .max(1)
        });

        self.events
            .publish(Event::AgentStarted {
                run: self.run_id,
                role: role.clone(),
                subtask: self.subtask,
            })
            .await;
        tracing::debug!(role = %role, tools = ?selected, "agent run started");

        let mut steps: u32 = 0;
        let mut tool_calls: u32 = 0;
        let mut usage = Usage::default();
        let mut warned_context = false;
        let mut converged = false;
        // Index of the assistant message cut by the output limit, while the
        // model is continuing it after a nudge.
        let mut cut_from: Option<usize> = None;

        let stop = loop {
            if self.is_cancelled() {
                break AgentStop::Cancelled;
            }
            if steps >= spec.max_steps {
                break AgentStop::MaxSteps;
            }
            if let Some(at) = converge_at
                && !converged
                && steps > 0
                && steps >= at
            {
                push_user_text(&mut messages, CONVERGE_MESSAGE.to_string());
                converged = true;
            }

            let estimate = estimate_tokens(&system, &messages) + tools_tokens;
            if estimate * 100 >= self.context_window * CONTEXT_STOP_PERCENT {
                tracing::debug!(estimate, "context window exhausted");
                break AgentStop::ContextWindow;
            }
            if !warned_context && estimate * 100 >= self.context_window * CONTEXT_WARNING_PERCENT {
                push_user_text(&mut messages, CONTEXT_WARNING_MESSAGE.to_string());
                warned_context = true;
            }

            let request = CompletionRequest {
                system: system.clone(),
                tools: tool_specs.clone(),
                max_tokens,
                thinking_budget,
                ..CompletionRequest::new(self.model.clone(), messages.clone())
            };
            let response = match self.complete_with_retry(request).await {
                Ok(r) => r,
                Err(e) if e.kind == ErrorKind::ContextTooLong => break AgentStop::ContextWindow,
                Err(e) if e.kind == ErrorKind::Cancelled => break AgentStop::Cancelled,
                Err(e) => {
                    break AgentStop::Error {
                        kind: e.kind,
                        message: e.message,
                    };
                }
            };
            steps += 1;
            usage += response.usage;
            if let Some(budget) = &self.budget {
                budget.add(response.usage);
            }

            let mut assistant = response.message;
            assistant.role = Role::Assistant;
            let text = assistant.text();
            if !text.trim().is_empty() {
                self.events
                    .publish(Event::AgentText {
                        run: self.run_id,
                        role: role.clone(),
                        text,
                    })
                    .await;
            }
            let calls: Vec<ToolCall> = assistant
                .tool_uses()
                .map(|(id, name, input)| ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: input.clone(),
                })
                .collect();
            messages.push(assistant);

            if !calls.is_empty() {
                // Tool use ends any continuation of a cut answer.
                cut_from = None;
                tool_calls += u32::try_from(calls.len()).unwrap_or(u32::MAX);
                let results = self.execute_tools(&role, &tools, &ctx, calls).await;
                messages.push(Message::tool_results(results));
                continue;
            }

            match response.stop_reason {
                StopReason::MaxTokens if cut_from.is_some() => {
                    break AgentStop::Error {
                        kind: ErrorKind::Other,
                        message: TRUNCATED_TWICE_MESSAGE.to_string(),
                    };
                }
                StopReason::MaxTokens => {
                    cut_from = Some(messages.len() - 1);
                    messages.push(Message::user(CONTINUE_NUDGE));
                }
                _ => break AgentStop::Completed,
            }
        };

        let final_text = match cut_from {
            // The answer was cut and continued: stitch the pieces together.
            Some(start) => concat_assistant_text(&messages[start..]),
            None => messages
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .map(Message::text)
                .unwrap_or_default(),
        };

        self.events
            .publish(Event::AgentFinished {
                run: self.run_id,
                role: role.clone(),
                steps,
                usage,
                stop: serde_json::to_string(&stop).unwrap_or_default(),
            })
            .await;
        tracing::debug!(role = %role, steps, tool_calls, ?stop, "agent run finished");

        Ok(AgentOutcome {
            role,
            stop,
            final_text,
            messages,
            steps,
            tool_calls,
            usage,
        })
    }

    async fn complete_with_retry(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let mut attempt: u32 = 0;
        loop {
            match self.provider.complete(request.clone()).await {
                Ok(r) => return Ok(r),
                Err(e) if e.is_retryable() && attempt < self.max_provider_retries => {
                    attempt += 1;
                    let delay = e
                        .retry_after
                        .unwrap_or_else(|| {
                            self.retry_base_delay
                                .saturating_mul(2u32.saturating_pow(attempt - 1))
                        })
                        .min(MAX_RETRY_DELAY);
                    self.events
                        .publish(Event::Retrying {
                            run: self.run_id,
                            what: format!("model call ({}): {}", self.model, e.message),
                            attempt,
                            delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        })
                        .await;
                    self.sleep_unless_cancelled(delay).await;
                    if self.is_cancelled() {
                        return Err(Error::new(ErrorKind::Cancelled, "cancelled during retry"));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Sleep for `delay`, waking up early when the cancel token flips.
    async fn sleep_unless_cancelled(&self, delay: Duration) {
        if delay.is_zero() {
            return;
        }
        let Some(rx) = &self.cancel else {
            tokio::time::sleep(delay).await;
            return;
        };
        let mut rx = rx.clone();
        let cancelled = async move {
            // A dropped sender can never cancel: wait out the full delay.
            if rx.wait_for(|c| *c).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = cancelled => {}
        }
    }

    /// Execute the calls of one step. Consecutive read-only calls run
    /// concurrently; a mutating call runs alone, after everything before it
    /// and before everything after it. Results keep the order of the calls.
    async fn execute_tools(
        &self,
        role: &AgentRole,
        tools: &ToolRegistry,
        ctx: &ToolContext,
        calls: Vec<ToolCall>,
    ) -> Vec<ContentBlock> {
        let is_mutating = |c: &ToolCall| tools.get(&c.name).is_some_and(|t| t.is_mutating());
        let mut results = Vec::with_capacity(calls.len());
        let mut i = 0;
        while i < calls.len() {
            if is_mutating(&calls[i]) {
                results.push(self.execute_one(role, tools, ctx, &calls[i]).await);
                i += 1;
            } else {
                let start = i;
                while i < calls.len() && !is_mutating(&calls[i]) {
                    i += 1;
                }
                let batch = calls[start..i]
                    .iter()
                    .map(|c| self.execute_one(role, tools, ctx, c));
                results.extend(join_all(batch).await);
            }
        }
        results
    }

    async fn execute_one(
        &self,
        role: &AgentRole,
        tools: &ToolRegistry,
        ctx: &ToolContext,
        call: &ToolCall,
    ) -> ContentBlock {
        self.events
            .publish(Event::ToolCalled {
                run: self.run_id,
                role: role.clone(),
                tool: call.name.clone(),
                input: call.input.clone(),
            })
            .await;
        let started = Instant::now();

        let raw_arguments = call
            .input
            .as_object()
            .is_some_and(|o| o.contains_key(RAW_ARGUMENTS_KEY));
        let output = match tools.get(&call.name) {
            // Unparsable arguments: never run the tool (nor its hooks).
            _ if raw_arguments => ToolOutput::error(format!(
                "Tool `{}` was not executed: {INVALID_ARGUMENTS_MESSAGE}.",
                call.name
            )),
            None => {
                let available: Vec<&str> = tools.names().collect();
                ToolOutput::error(format!(
                    "Unknown tool `{}`. Available tools: {}.",
                    call.name,
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ))
            }
            Some(tool) => match self
                .registry
                .before_tool(ctx, &call.name, &call.input)
                .await
            {
                HookDecision::Abort(reason) => ToolOutput::error(format!(
                    "The call to `{}` was blocked by a policy hook: {reason}. \
                     Do not repeat this call; choose another approach.",
                    call.name
                )),
                HookDecision::Continue => {
                    let fut = tool.call(ctx, call.input.clone());
                    let output = match AssertUnwindSafe(fut).catch_unwind().await {
                        Ok(Ok(out)) => out,
                        Ok(Err(e)) => {
                            ToolOutput::error(format!("Tool `{}` failed: {}", call.name, e.message))
                        }
                        Err(panic) => ToolOutput::error(format!(
                            "Tool `{}` crashed: {}",
                            call.name,
                            panic_message(panic.as_ref())
                        )),
                    };
                    self.registry.after_tool(ctx, &call.name, &output).await;
                    output
                }
            },
        };
        let output = self.truncate_output(ctx, output).await;

        self.events
            .publish(Event::ToolReturned {
                run: self.run_id,
                role: role.clone(),
                tool: call.name.clone(),
                is_error: output.is_error,
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                preview: truncate_chars(&output.content, PREVIEW_CHARS).to_string(),
            })
            .await;

        ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            content: output.content,
            is_error: output.is_error,
        }
    }

    /// Truncate an oversized output, saving the full text to
    /// `.vibe/tool-output/` under the workspace root.
    async fn truncate_output(&self, ctx: &ToolContext, mut output: ToolOutput) -> ToolOutput {
        let total = output.content.chars().count();
        if total <= self.max_tool_output_chars {
            return output;
        }
        let saved = save_full_output(&ctx.workspace_root, &output.content).await;
        let kept = truncate_chars(&output.content, self.max_tool_output_chars).to_string();
        let note = match saved {
            Ok(path) => format!(
                "\n\n[Output truncated: showing the first {} of {total} characters. \
                 The full output was saved to `{}`; read it in smaller pieces if you need more.]",
                self.max_tool_output_chars,
                path.display()
            ),
            Err(e) => format!(
                "\n\n[Output truncated: showing the first {} of {total} characters. \
                 The full output could not be saved: {}]",
                self.max_tool_output_chars, e.message
            ),
        };
        output.content = kept + &note;
        output
    }
}

/// Whether the agent should be told to converge at 75% of its step budget.
fn needs_convergence(spec: &AgentSpec) -> bool {
    spec.structured_output || matches!(spec.role, AgentRole::QaReviewer | AgentRole::SpecCritic)
}

/// Output budget of a step: providers with extended thinking require the
/// output budget to exceed the thinking budget, so leave room for an answer.
fn effective_max_tokens(max_tokens: u32, thinking: ThinkingLevel) -> u32 {
    match thinking.budget() {
        Some(budget) => max_tokens.max(budget.saturating_add(4096)),
        None => max_tokens,
    }
}

/// Every text block of the assistant messages in `messages`, concatenated in
/// order without separator (used to stitch an answer cut by the output
/// limit back together).
fn concat_assistant_text(messages: &[Message]) -> String {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Token estimate of a prompt (system + messages).
fn estimate_tokens(system: &str, messages: &[Message]) -> usize {
    system.len().div_ceil(4) + messages.iter().map(Message::estimate_tokens).sum::<usize>()
}

/// Append a user text, merging it into the last message when that message is
/// already from the user (providers reject two consecutive user turns).
pub(crate) fn push_user_text(messages: &mut Vec<Message>, text: String) {
    match messages.last_mut() {
        Some(last) if last.role == Role::User => last.content.push(ContentBlock::Text { text }),
        _ => messages.push(Message::user(text)),
    }
}

/// Longest prefix of `s` with at most `max` characters.
pub(crate) fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

async fn save_full_output(root: &std::path::Path, content: &str) -> Result<PathBuf> {
    let dir = root.join(".vibe").join("tool-output");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}.txt", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, content).await?;
    Ok(path)
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_user_text_merges_into_user_turn() {
        let mut m = vec![Message::user("a")];
        push_user_text(&mut m, "b".into());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text(), "a\nb");
        m.push(Message::assistant("x"));
        push_user_text(&mut m, "c".into());
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn truncate_chars_respects_boundaries() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn max_tokens_leaves_room_for_thinking() {
        assert_eq!(effective_max_tokens(8192, ThinkingLevel::Off), 8192);
        assert_eq!(effective_max_tokens(8192, ThinkingLevel::Low), 8192);
        assert!(effective_max_tokens(8192, ThinkingLevel::High) > 16384);
    }

    #[test]
    fn convergence_roles() {
        assert!(needs_convergence(&AgentSpec::new(
            AgentRole::QaReviewer,
            ""
        )));
        assert!(needs_convergence(&AgentSpec::new(
            AgentRole::SpecCritic,
            ""
        )));
        assert!(!needs_convergence(&AgentSpec::new(AgentRole::Coder, "")));
        assert!(needs_convergence(
            &AgentSpec::new(AgentRole::Coder, "").with_structured_output()
        ));
    }

    #[test]
    fn panic_messages() {
        let p: Box<dyn std::any::Any + Send> = Box::new("oops");
        assert_eq!(panic_message(p.as_ref()), "oops");
        let p: Box<dyn std::any::Any + Send> = Box::new(String::from("owned"));
        assert_eq!(panic_message(p.as_ref()), "owned");
        let p: Box<dyn std::any::Any + Send> = Box::new(1u8);
        assert_eq!(panic_message(p.as_ref()), "unknown panic");
    }
}
