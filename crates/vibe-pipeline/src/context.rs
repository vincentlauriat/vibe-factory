//! The state shared by the phases of one run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::watch;
use vibe_agents::AgentRunner;
use vibe_core::{
    AgentOutcome, AgentRole, AgentSpec, AgentStop, Complexity, Error, ErrorKind, EventBus,
    ModelProvider, ModelRef, ModelSelection, Permissions, Phase, Plan, Registry, Result, RunId,
    SharedProvider, Spec, Task, TaskStatus, ToolContext, ToolRegistry, Usage, VibeConfig,
    Workspace, WorkspaceProvider,
};

use crate::complexity::Profile;
use crate::kickoff::{
    DOCUMENT_MAX_CHARS, MEMORY_MAX_CHARS, PROGRESS_MAX_CHARS, truncate_head, truncate_tail,
};
use crate::state::RunState;
use crate::store::{PipelineStore, plan_to_markdown};

/// Maps a [`ModelRef`] to a provider and the provider-specific model id.
///
/// The CLI adapts its provider registry to this trait; tests implement it
/// with a scripted provider. [`RegistryResolver`] uses the providers of a
/// [`Registry`].
pub trait ProviderResolver: Send + Sync {
    /// Resolve `model` into `(provider, model id)`.
    ///
    /// An empty `model.model` (as produced by `ModelRef::parse("ollama/")`)
    /// means "the provider's default model": implementations return the
    /// provider's default id, or an empty string when the provider itself
    /// substitutes its default.
    fn resolve(&self, model: &ModelRef) -> Result<(Arc<dyn ModelProvider>, String)>;
}

/// [`ProviderResolver`] looking providers up by name in a [`Registry`]. An
/// empty model resolves to the provider's `info().default_model`.
#[derive(Clone, Default)]
pub struct RegistryResolver {
    providers: BTreeMap<String, SharedProvider>,
}

impl std::fmt::Debug for RegistryResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.providers.keys()).finish()
    }
}

impl RegistryResolver {
    /// Resolver over the providers registered in `registry`.
    #[must_use]
    pub fn new(registry: &Registry) -> Self {
        Self {
            providers: registry.providers.clone(),
        }
    }

    /// Add (or replace) a provider.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, provider: SharedProvider) -> Self {
        self.providers.insert(name.into(), provider);
        self
    }
}

impl ProviderResolver for RegistryResolver {
    fn resolve(&self, model: &ModelRef) -> Result<(Arc<dyn ModelProvider>, String)> {
        self.providers
            .get(&model.provider)
            .map(|p| {
                let id = if model.model.trim().is_empty() {
                    p.info().default_model
                } else {
                    model.model.clone()
                };
                (Arc::clone(p), id)
            })
            .ok_or_else(|| {
                Error::config(format!(
                    "no provider named `{}` for model `{model}`",
                    model.provider
                ))
            })
    }
}

/// Commits the work left in a workspace: `(workspace root, message)` to the
/// new commit id, or `None` when there was nothing to commit.
///
/// The CLI plugs the git implementation in:
///
/// ```
/// # use std::path::Path;
/// # use std::sync::Arc;
/// # use vibe_pipeline::Committer;
/// # // Stand-in with the signature of the git implementation (`commit_all`).
/// # async fn commit_all(_root: &Path, _msg: &str, _exclude: &[&str]) -> vibe_core::Result<Option<String>> { Ok(None) }
/// let committer: Committer = Arc::new(|root, message| {
///     Box::pin(async move { commit_all(&root, &message, &[]).await })
/// });
/// ```
pub type Committer =
    Arc<dyn Fn(PathBuf, String) -> BoxFuture<'static, Result<Option<String>>> + Send + Sync>;

/// What the pipeline does after a phase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Transition {
    /// Go to the next phase of the profile.
    Continue,
    /// Jump to a given phase (QA → Fix, Fix → QA).
    Goto {
        /// Target phase.
        phase: Phase,
    },
    /// End the run with a final task status.
    Stop {
        /// Final status of the task.
        status: TaskStatus,
        /// Why the run stops.
        reason: String,
        /// Whether the run waits for a human (publishes `Paused` and leaves
        /// the run resumable).
        pause: bool,
    },
}

/// Result of one phase execution.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhaseResult {
    /// Phase that ran.
    pub phase: Phase,
    /// Whether the phase reached its goal (for QA: the verdict is approved).
    pub success: bool,
    /// One-line summary.
    pub summary: String,
    /// Tokens used by the phase.
    pub usage: Usage,
    /// What happens next.
    pub next: Transition,
}

impl PhaseResult {
    /// Successful result that continues with the next phase.
    pub fn ok(phase: Phase, summary: impl Into<String>) -> Self {
        Self {
            phase,
            success: true,
            summary: summary.into(),
            usage: Usage::default(),
            next: Transition::Continue,
        }
    }

    /// Set the transition.
    #[must_use]
    pub fn then(mut self, next: Transition) -> Self {
        self.next = next;
        self
    }

    /// Set the success flag.
    #[must_use]
    pub fn with_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }
}

/// Everything a phase needs: the task and its artefacts, the injected
/// services, and the accounting of the run.
pub struct RunContext {
    /// Run identifier.
    pub run_id: RunId,
    /// Task being run (phases update it; the pipeline saves it).
    pub task: Task,
    /// Workspace the agents work in.
    pub workspace: Workspace,
    /// Pipeline profile (set by the assessment).
    pub profile: Profile,
    /// Persistent run state.
    pub state: RunState,
    /// Configuration.
    pub config: Arc<VibeConfig>,
    /// Plugins, agents and hooks.
    pub registry: Arc<Registry>,
    /// Model resolution.
    pub providers: Arc<dyn ProviderResolver>,
    /// Persistence.
    pub store: Arc<dyn PipelineStore>,
    /// Workspace integration (changes, merge).
    pub workspace_provider: Arc<dyn WorkspaceProvider>,
    /// Every tool available to agents.
    pub tools: ToolRegistry,
    /// Event bus.
    pub events: EventBus,
    /// Optional per-subtask committer.
    pub committer: Option<Committer>,
    /// Cancellation token.
    pub cancel: Option<watch::Receiver<bool>>,
    /// Complexity forced by the caller.
    pub complexity_override: Option<Complexity>,
    /// Current specification, once written or loaded.
    pub spec: Option<Spec>,
    /// Current plan, once written or loaded.
    pub plan: Option<Plan>,
    /// Phase being executed.
    pub phase: Phase,
    /// Tokens used so far in this invocation.
    pub usage: Usage,
    /// QA reviews performed in this invocation (the round limit applies to
    /// this counter, so a resumed run gets a fresh budget).
    pub qa_rounds_this_invocation: u32,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("run_id", &self.run_id)
            .field("task", &self.task.id)
            .field("phase", &self.phase)
            .field("profile", &self.profile)
            .field("workspace", &self.workspace.root)
            .finish_non_exhaustive()
    }
}

impl RunContext {
    /// Whether the run has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|rx| *rx.borrow())
    }

    /// Agent spec for `role`: the registry's (plugins and overrides win),
    /// else the built-in one. When the configuration has an explicit entry
    /// for the current phase, its thinking level replaces the agent's.
    pub fn agent_spec(&self, role: &AgentRole) -> Result<AgentSpec> {
        let mut spec = self
            .registry
            .agent(role)
            .cloned()
            .or_else(|| vibe_agents::builtin_agent(role))
            .ok_or_else(|| Error::config(format!("no agent registered for role `{role}`")))?;
        if let Some(pm) = self.config.phases.get(self.phase) {
            spec.thinking = pm.thinking;
        }
        Ok(spec)
    }

    /// Tool permissions granted by the configuration.
    #[must_use]
    pub fn permissions(&self) -> Permissions {
        Permissions {
            read: true,
            write: true,
            execute: true,
            network: self.config.security.allow_network,
            extra_read_paths: self.config.security.extra_read_paths.clone(),
        }
    }

    /// Agent runner for `spec`: model from the spec when fixed, else the
    /// model of the current phase; tools confined to the workspace.
    pub fn runner(&self, spec: &AgentSpec) -> Result<AgentRunner> {
        let model = match &spec.model {
            ModelSelection::Fixed(m) => m.clone(),
            ModelSelection::Phase => self.config.model_for(self.phase).0,
        };
        let (provider, model_id) = self.providers.resolve(&model)?;
        let mut runner = AgentRunner::new(
            provider,
            model_id,
            self.tools.clone(),
            Arc::clone(&self.registry),
            self.events.clone(),
        )
        .run_id(self.run_id)
        .task(self.task.clone())
        .tool_context(ToolContext::new(self.workspace.root.clone()))
        .permissions(self.permissions());
        if let Some(c) = &self.cancel {
            runner = runner.cancel_token(c.clone());
        }
        Ok(runner)
    }

    /// Add an outcome's usage to the run total.
    pub fn record(&mut self, outcome: &AgentOutcome) {
        self.usage += outcome.usage;
    }

    /// `Err(Cancelled)` when the outcome was cancelled.
    pub fn check_cancelled(outcome: &AgentOutcome) -> Result<()> {
        if outcome.stop == AgentStop::Cancelled {
            return Err(Error::new(ErrorKind::Cancelled, "run cancelled"));
        }
        Ok(())
    }

    /// Rendered specification (or a note for spec-less profiles).
    #[must_use]
    pub fn spec_text(&self) -> String {
        match &self.spec {
            Some(s) => truncate_head(&s.to_markdown(), DOCUMENT_MAX_CHARS),
            None => "No written specification (quick profile): work from the task \
                     description above and treat it as the only requirement."
                .to_string(),
        }
    }

    /// Rendered plan, with subtask statuses.
    #[must_use]
    pub fn plan_text(&self) -> String {
        self.plan
            .as_ref()
            .map(|p| truncate_head(&plan_to_markdown(p), DOCUMENT_MAX_CHARS))
            .unwrap_or_default()
    }

    /// Most recent progress notes.
    pub async fn progress_text(&self) -> Result<String> {
        let p = self.store.load_progress(self.task.id).await?;
        Ok(truncate_tail(&p, PROGRESS_MAX_CHARS))
    }

    /// Task memory files plus entries recalled from registered memory
    /// stores.
    pub async fn memory_text(&self) -> Result<String> {
        let mut out = self.store.load_memory(self.task.id).await?;
        let query = format!("{} {}", self.task.title, self.task.description);
        for (name, mem) in &self.registry.memories {
            match mem.recall(&query, 10).await {
                Ok(entries) if !entries.is_empty() => {
                    out.push_str(&format!("\n## Recalled from `{name}`\n\n"));
                    for e in entries {
                        out.push_str(&format!("- ({:?}) {}\n", e.kind, e.content));
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(memory = %name, error = %e, "memory recall failed"),
            }
        }
        Ok(truncate_head(&out, MEMORY_MAX_CHARS))
    }

    /// Append a progress note.
    pub async fn note(&self, text: &str) -> Result<()> {
        self.store.append_progress(self.task.id, text).await
    }

    /// Commit the workspace through the injected committer, if any. A
    /// failed commit is logged and noted, never fatal.
    pub async fn commit(&self, message: &str) -> Option<String> {
        let committer = self.committer.as_ref()?;
        match committer(self.workspace.root.clone(), message.to_string()).await {
            Ok(sha) => sha,
            Err(e) => {
                self.events
                    .log(Some(self.run_id), "warn", format!("commit failed: {e}"))
                    .await;
                let _ = self.note(&format!("Commit `{message}` failed: {e}")).await;
                None
            }
        }
    }
}

/// `after - before`, field by field.
#[must_use]
pub fn usage_delta(after: Usage, before: Usage) -> Usage {
    Usage {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        cache_read_tokens: after
            .cache_read_tokens
            .saturating_sub(before.cache_read_tokens),
        cache_write_tokens: after
            .cache_write_tokens
            .saturating_sub(before.cache_write_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_agents::test_support::ScriptedProvider;

    #[test]
    fn registry_resolver() {
        let r = RegistryResolver::default().with("s", Arc::new(ScriptedProvider::new(vec![])));
        let (_, m) = r.resolve(&ModelRef::new("s", "m1")).unwrap();
        assert_eq!(m, "m1");
        let (_, m) = r.resolve(&ModelRef::parse("s/", "x")).unwrap();
        assert_eq!(m, "scripted", "empty model means the provider default");
        assert!(r.resolve(&ModelRef::new("x", "m")).is_err());
    }

    async fn borrowing_commit(
        root: &std::path::Path,
        message: &str,
        _exclude: &[&str],
    ) -> Result<Option<String>> {
        Ok(Some(format!("{}:{message}", root.display())))
    }

    #[tokio::test]
    async fn committer_accepts_borrowing_async_fn() {
        // Same signature as the git implementation's `commit_all`.
        let c: Committer = Arc::new(|root, message| {
            Box::pin(async move { borrowing_commit(&root, &message, &[]).await })
        });
        let sha = c(PathBuf::from("/w"), "msg".into()).await.unwrap();
        assert_eq!(sha.as_deref(), Some("/w:msg"));
    }

    #[test]
    fn delta() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        };
        let d = usage_delta(a, Usage::default());
        assert_eq!(d.total(), 15);
        assert_eq!(usage_delta(Usage::default(), a).total(), 0);
    }
}
