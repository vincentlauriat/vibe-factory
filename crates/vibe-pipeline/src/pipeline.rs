//! The pipeline driver: runs the phases of a profile for a task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OnceCell, watch};
use vibe_core::{
    Complexity, Envelope, Error, ErrorKind, Event, EventBus, EventSink, HookDecision, Phase,
    Registry, Result, RunId, SubtaskStatus, Task, TaskId, TaskStatus, ToolRegistry, Usage,
    VibeConfig, WorkspaceProvider,
};

use crate::complexity::{Profile, heuristic_complexity, profile_for};
use crate::context::{
    Committer, PhaseResult, ProviderResolver, Resetter, RunContext, Transition, usage_delta,
};
use crate::phases;
use crate::state::{RunState, RunStatus};
use crate::store::PipelineStore;

/// Everything the pipeline depends on. Concrete providers, tools and
/// workspaces are injected here; this crate never builds them itself.
pub struct PipelineDeps {
    /// Plugins, agent specs (overrides of the built-in agents) and hooks.
    pub registry: Arc<Registry>,
    /// Model resolution.
    pub providers: Arc<dyn ProviderResolver>,
    /// Persistence of tasks, artefacts and run state.
    pub store: Arc<dyn PipelineStore>,
    /// Workspace isolation and merge.
    pub workspace: Arc<dyn WorkspaceProvider>,
    /// Tools offered to agents (merged with the registry's tools).
    pub tools: ToolRegistry,
    /// Event bus.
    pub events: EventBus,
    /// Configuration.
    pub config: VibeConfig,
    /// Project directory.
    pub project_root: PathBuf,
    /// Commits the workspace after each successful subtask and QA fix.
    pub committer: Option<Committer>,
    /// Discards the changes of a failed subtask attempt (see [`Resetter`]).
    pub resetter: Option<Resetter>,
}

impl std::fmt::Debug for PipelineDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineDeps")
            .field("project_root", &self.project_root)
            .field("workspace", &self.workspace.name())
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

/// Options of one run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Force a complexity (skips the heuristic and the assessor).
    pub complexity_override: Option<Complexity>,
    /// Start at this phase instead of `assess`, reusing persisted artefacts.
    pub from_phase: Option<Phase>,
    /// Stop (paused, resumable) once this phase is done.
    pub until_phase: Option<Phase>,
    /// Only assess, specify and plan (same as `until_phase = plan`).
    pub dry_run: bool,
    /// Cancellation token: the run stops when the value becomes `true`.
    pub cancel: Option<watch::Receiver<bool>>,
}

/// Result of a run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunReport {
    /// Run identifier.
    pub run_id: RunId,
    /// Task, as saved at the end of the run.
    pub task: Task,
    /// Final task status.
    pub final_status: TaskStatus,
    /// Every phase executed, in order.
    pub phases: Vec<PhaseResult>,
    /// Tokens used by this invocation.
    pub usage: Usage,
    /// Wall-clock duration of this invocation.
    pub duration: Duration,
    /// Final state of the run (`run.json`).
    pub state: RunState,
}

impl RunReport {
    /// Whether the task ended in a successful state (ready or done).
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self.final_status, TaskStatus::Ready | TaskStatus::Done)
    }
}

/// Routes events to the per-run sinks of the store. Registered once on the
/// bus (the bus has no way to remove a sink).
#[derive(Default)]
struct EventRouter {
    routes: Mutex<HashMap<RunId, Arc<dyn EventSink>>>,
}

impl EventRouter {
    fn set(&self, run: RunId, sink: Option<Arc<dyn EventSink>>) {
        let mut routes = self.routes.lock().unwrap_or_else(|e| e.into_inner());
        match sink {
            Some(s) => {
                routes.insert(run, s);
            }
            None => {
                routes.remove(&run);
            }
        }
    }
}

#[async_trait::async_trait]
impl EventSink for EventRouter {
    async fn on_event(&self, envelope: &Envelope) {
        let Some(run) = envelope.event.run_id() else {
            return;
        };
        let sink = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run)
            .cloned();
        if let Some(sink) = sink {
            sink.on_event(envelope).await;
        }
    }
}

/// Task status while `phase` runs.
fn status_for(phase: Phase) -> TaskStatus {
    match phase {
        Phase::Assess | Phase::Spec | Phase::Plan => TaskStatus::Planning,
        Phase::Build => TaskStatus::Building,
        Phase::Qa | Phase::Fix | Phase::Merge => TaskStatus::Review,
    }
}

/// Runs tasks through the multi-agent pipeline.
pub struct Pipeline {
    deps: PipelineDeps,
    tools: ToolRegistry,
    config: Arc<VibeConfig>,
    router: Arc<EventRouter>,
    router_installed: OnceCell<()>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("deps", &self.deps)
            .finish_non_exhaustive()
    }
}

enum Flow {
    Next(Option<Phase>),
    End(TaskStatus, RunStatus),
}

impl Pipeline {
    /// Build a pipeline from its dependencies.
    #[must_use]
    pub fn new(deps: PipelineDeps) -> Self {
        let mut tools = deps.registry.tools.clone();
        tools.extend(&deps.tools);
        let config = Arc::new(deps.config.clone());
        Self {
            deps,
            tools,
            config,
            router: Arc::new(EventRouter::default()),
            router_installed: OnceCell::new(),
        }
    }

    /// The store used by the pipeline.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn PipelineStore> {
        &self.deps.store
    }

    /// The event bus used by the pipeline.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.deps.events
    }

    /// Run a task from `options.from_phase` (default: `assess`).
    pub async fn run(&self, task_id: TaskId, options: RunOptions) -> Result<RunReport> {
        let task = self.deps.store.load_task(task_id).await?;
        let start = options.from_phase.unwrap_or(Phase::Assess);
        let mut state = RunState::new(RunId::new(), task_id, start);
        // Keep the profile of an earlier run when starting mid-pipeline.
        if start != Phase::Assess
            && options.complexity_override.is_none()
            && let Some(prev) = self.deps.store.load_run_state(task_id).await?
        {
            state.profile = prev.profile;
        }
        self.execute(task, state, options).await
    }

    /// Resume the last run of a task from its persisted state.
    pub async fn resume(&self, task_id: TaskId) -> Result<RunReport> {
        self.resume_with(task_id, RunOptions::default()).await
    }

    /// Like [`Pipeline::resume`] with options (cancellation, `until_phase`).
    /// `from_phase` defaults to the phase the run stopped at.
    pub async fn resume_with(&self, task_id: TaskId, mut options: RunOptions) -> Result<RunReport> {
        let task = self.deps.store.load_task(task_id).await?;
        let mut state = self
            .deps
            .store
            .load_run_state(task_id)
            .await?
            .ok_or_else(|| Error::config(format!("task {task_id} has no run to resume")))?;
        if !state.status.is_resumable() {
            return Err(Error::config(format!(
                "the last run of task {task_id} already finished; start a new run"
            )));
        }
        if options.from_phase.is_none() {
            options.from_phase = Some(state.current_phase);
        }
        // Retrying a failed build: give failed and skipped subtasks a fresh
        // budget (done subtasks are kept).
        if state.status == RunStatus::Failed
            && options.from_phase == Some(Phase::Build)
            && let Some(mut plan) = self.deps.store.load_plan(task_id).await?
        {
            let mut reset = 0;
            for s in plan.subtasks_mut() {
                if matches!(s.status, SubtaskStatus::Failed | SubtaskStatus::Skipped) {
                    s.status = SubtaskStatus::Pending;
                    s.attempts = 0;
                    reset += 1;
                }
            }
            if reset > 0 {
                self.deps.store.save_plan(&plan).await?;
                self.deps
                    .store
                    .append_progress(
                        task_id,
                        &format!("Resume: {reset} failed or skipped subtask(s) will be retried."),
                    )
                    .await?;
            }
        }
        state.status = RunStatus::Running;
        state.last_error = None;
        self.execute(task, state, options).await
    }

    async fn execute(
        &self,
        task: Task,
        state: RunState,
        mut options: RunOptions,
    ) -> Result<RunReport> {
        let started = Instant::now();
        if options.dry_run {
            options.until_phase = Some(
                options
                    .until_phase
                    .map_or(Phase::Plan, |u| u.min(Phase::Plan)),
            );
        }
        let store = Arc::clone(&self.deps.store);
        let events = self.deps.events.clone();
        let run_id = state.run_id;

        self.router_installed
            .get_or_init(|| async {
                let router: Arc<dyn EventSink> = self.router.clone();
                events.add_sink(router).await;
            })
            .await;
        self.router
            .set(run_id, store.event_sink(task.id, run_id).await?);

        events
            .publish(Event::RunStarted {
                run: run_id,
                task: task.id,
            })
            .await;

        let workspace = match self
            .deps
            .workspace
            .open(&self.deps.project_root, &task)
            .await
        {
            Ok(w) => w,
            Err(e) => {
                events
                    .publish(Event::RunFinished {
                        run: run_id,
                        success: false,
                        status: task.status,
                    })
                    .await;
                self.router.set(run_id, None);
                return Err(e);
            }
        };
        let spec = store.load_spec(task.id).await?;
        let plan = store.load_plan(task.id).await?;

        let start = options.from_phase.unwrap_or(Phase::Assess);
        let profile = state.profile.clone().unwrap_or_else(|| {
            profile_for(
                options
                    .complexity_override
                    .or(task.complexity)
                    .or_else(|| heuristic_complexity(&task))
                    .unwrap_or(Complexity::Standard),
            )
        });
        let mut ctx = RunContext {
            run_id,
            task,
            workspace,
            profile,
            state,
            config: Arc::clone(&self.config),
            registry: Arc::clone(&self.deps.registry),
            providers: Arc::clone(&self.deps.providers),
            store: Arc::clone(&store),
            workspace_provider: Arc::clone(&self.deps.workspace),
            tools: self.tools.clone(),
            events: events.clone(),
            committer: self.deps.committer.clone(),
            resetter: self.deps.resetter.clone(),
            cancel: options.cancel.clone(),
            complexity_override: options.complexity_override,
            spec,
            plan,
            phase: start,
            usage: Usage::default(),
            qa_rounds_this_invocation: 0,
        };
        if start != Phase::Assess {
            // Starting mid-pipeline: settle the complexity without the assessor.
            if ctx.task.complexity.is_none() || options.complexity_override.is_some() {
                ctx.task.complexity = options
                    .complexity_override
                    .or(ctx.task.complexity)
                    .or_else(|| heuristic_complexity(&ctx.task))
                    .or(Some(Complexity::Standard));
            }
            if ctx.state.profile.is_none() {
                ctx.state.profile = Some(ctx.profile.clone());
            }
        }

        let mut phases_run: Vec<PhaseResult> = Vec::new();
        let mut current = Some(start);
        let (final_status, run_status) = loop {
            let Some(phase) = current else {
                break (ctx.task.status, RunStatus::Finished);
            };
            let pending_fix = phase == Phase::Fix && ctx.state.pending_validation_fix.is_some();
            if !(ctx.profile.has(phase) || pending_fix) {
                current = ctx.profile.next_after(phase);
                continue;
            }
            if ctx.is_cancelled() {
                ctx.state.current_phase = phase;
                ctx.state.last_error = Some("cancelled".into());
                break (TaskStatus::Cancelled, RunStatus::Cancelled);
            }
            ctx.phase = phase;
            ctx.state.current_phase = phase;
            ctx.state.status = RunStatus::Running;
            ctx.state.touch();
            ctx.task.set_status(status_for(phase));
            store.save_task(&ctx.task).await?;
            store.save_run_state(&ctx.state).await?;

            if let HookDecision::Abort(reason) = ctx.registry.before_phase(phase, &ctx.task).await {
                let msg = format!("phase {phase} aborted by hook {reason}");
                events.log(Some(run_id), "warn", msg.clone()).await;
                let _ = ctx.note(&msg).await;
                ctx.state.last_error = Some(msg);
                break (TaskStatus::Cancelled, RunStatus::Cancelled);
            }

            events
                .publish(Event::PhaseStarted { run: run_id, phase })
                .await;
            let before = ctx.usage;
            let result = match phase {
                Phase::Assess => phases::run_assess(&mut ctx).await,
                Phase::Spec => phases::run_spec(&mut ctx).await,
                Phase::Plan => phases::run_plan(&mut ctx).await,
                Phase::Build => phases::run_build(&mut ctx).await,
                Phase::Qa => phases::run_qa(&mut ctx).await,
                Phase::Fix => phases::run_fix(&mut ctx).await,
                Phase::Merge => phases::run_merge(&mut ctx).await,
            };
            let usage = usage_delta(ctx.usage, before);

            let flow = match result {
                Ok(mut r) => {
                    r.usage = usage;
                    events
                        .publish(Event::PhaseFinished {
                            run: run_id,
                            phase,
                            success: r.success,
                            summary: r.summary.clone(),
                        })
                        .await;
                    ctx.registry.after_phase(phase, &ctx.task, r.success).await;
                    ctx.state.completed_phases.push(phase);
                    let next = r.next.clone();
                    phases_run.push(r);
                    match next {
                        Transition::Continue => Flow::Next(ctx.profile.next_after(phase)),
                        Transition::Goto { phase: p } => Flow::Next(Some(p)),
                        Transition::Stop {
                            status,
                            reason,
                            pause,
                        } => {
                            if pause {
                                events
                                    .publish(Event::Paused {
                                        run: run_id,
                                        reason: reason.clone(),
                                    })
                                    .await;
                                ctx.state.last_error = Some(reason);
                                if phase == Phase::Fix {
                                    // A resumed run re-reviews before fixing again.
                                    ctx.state.current_phase = Phase::Qa;
                                }
                                Flow::End(status, RunStatus::Paused)
                            } else if status == TaskStatus::Failed {
                                ctx.state.last_error = Some(reason);
                                Flow::End(status, RunStatus::Failed)
                            } else {
                                Flow::End(status, RunStatus::Finished)
                            }
                        }
                    }
                }
                Err(e) => {
                    let cancelled = e.kind == ErrorKind::Cancelled;
                    events
                        .publish(Event::PhaseFinished {
                            run: run_id,
                            phase,
                            success: false,
                            summary: e.to_string(),
                        })
                        .await;
                    ctx.registry.after_phase(phase, &ctx.task, false).await;
                    phases_run.push(PhaseResult {
                        phase,
                        success: false,
                        summary: e.to_string(),
                        usage,
                        next: Transition::Stop {
                            status: if cancelled {
                                TaskStatus::Cancelled
                            } else {
                                TaskStatus::Failed
                            },
                            reason: e.message.clone(),
                            pause: false,
                        },
                    });
                    if !cancelled {
                        let _ = ctx.note(&format!("Phase {phase} failed: {e}")).await;
                    }
                    ctx.state.last_error = Some(e.to_string());
                    if cancelled {
                        Flow::End(TaskStatus::Cancelled, RunStatus::Cancelled)
                    } else {
                        Flow::End(TaskStatus::Failed, RunStatus::Failed)
                    }
                }
            };

            match flow {
                Flow::End(status, run_status) => break (status, run_status),
                Flow::Next(next) => {
                    // Skip phases the profile does not run.
                    let mut next = next;
                    while let Some(p) = next
                        && !ctx.profile.has(p)
                        && !(p == Phase::Fix && ctx.state.pending_validation_fix.is_some())
                    {
                        next = ctx.profile.next_after(p);
                    }
                    if let Some(until) = options.until_phase
                        && (phase >= until || next.is_some_and(|n| n > until))
                        && let Some(n) = next
                    {
                        ctx.state.current_phase = n;
                        let reason = format!("stopped after phase {phase} as requested");
                        let _ = ctx.note(&format!("Run paused: {reason}.")).await;
                        break (TaskStatus::Backlog, RunStatus::Paused);
                    }
                    if let Some(n) = next {
                        ctx.state.current_phase = n;
                    }
                    ctx.state.touch();
                    store.save_run_state(&ctx.state).await?;
                    current = next;
                }
            }
        };

        ctx.task.set_status(final_status);
        store.save_task(&ctx.task).await?;
        ctx.state.status = run_status;
        ctx.state.touch();
        store.save_run_state(&ctx.state).await?;
        events
            .publish(Event::RunFinished {
                run: run_id,
                success: matches!(final_status, TaskStatus::Ready | TaskStatus::Done),
                status: final_status,
            })
            .await;
        self.router.set(run_id, None);

        Ok(RunReport {
            run_id,
            task: ctx.task,
            final_status,
            phases: phases_run,
            usage: ctx.usage,
            duration: started.elapsed(),
            state: ctx.state,
        })
    }
}

/// Profile a task would get without calling the assessor (override, stored
/// complexity, heuristic, else standard). Useful for `--dry-run` displays.
#[must_use]
pub fn default_profile(task: &Task, complexity_override: Option<Complexity>) -> Profile {
    profile_for(
        complexity_override
            .or(task.complexity)
            .or_else(|| heuristic_complexity(task))
            .unwrap_or(Complexity::Standard),
    )
}
