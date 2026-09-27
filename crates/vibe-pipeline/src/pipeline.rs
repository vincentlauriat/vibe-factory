//! The pipeline driver: runs the phases of a profile for a task.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OnceCell, watch};
use vibe_core::{
    ApprovalGate, Complexity, Envelope, Error, ErrorKind, Event, EventBus, EventSink, HookDecision,
    Phase, QaIssue, QaReport, QaVerdict, Registry, Result, RunBudget, RunId, Severity,
    SubtaskStatus, Task, TaskId, TaskStatus, ToolRegistry, Usage, VibeConfig, WorkspaceProvider,
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
    /// Gives every subtask attempt its own workspace (see
    /// [`vibe_core::SubtaskWorkspaces`]); used when
    /// `pipeline.isolate_subtasks` is true. `None` shares the task
    /// workspace between parallel subtasks.
    pub subtask_workspaces: Option<Arc<dyn vibe_core::SubtaskWorkspaces>>,
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

/// Fold the usage and active time of this invocation into the persisted
/// totals of the run, which started the invocation at `base`.
fn sync_accounting(ctx: &mut RunContext, base: (Usage, u64), started: Instant) {
    ctx.state.usage = base.0.combined(ctx.usage);
    let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    ctx.state.active_ms = base.1.saturating_add(now);
}

/// How often a run checks for a cancellation requested by another process.
const CANCEL_POLL: Duration = Duration::from_millis(500);

/// Aborts a background task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The run's cancellation token: flips when the caller's token does or when
/// another process asks to cancel the task (see
/// [`PipelineStore::take_cancel_request`]). The watcher stops with the
/// returned guard.
fn cancel_token(
    caller: Option<watch::Receiver<bool>>,
    store: &Arc<dyn PipelineStore>,
    task: TaskId,
) -> (watch::Receiver<bool>, AbortOnDrop) {
    let (tx, rx) = watch::channel(caller.as_ref().is_some_and(|c| *c.borrow()));
    let store = Arc::clone(store);
    let watcher = tokio::spawn(async move {
        let mut caller = caller;
        loop {
            let caller_cancelled = async {
                match caller.as_mut() {
                    Some(c) => {
                        if c.changed().await.is_err() {
                            // The caller dropped its sender: it can never cancel.
                            std::future::pending::<()>().await;
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = caller_cancelled => {
                    if caller.as_ref().is_some_and(|c| *c.borrow()) {
                        let _ = tx.send(true);
                        return;
                    }
                }
                () = tokio::time::sleep(CANCEL_POLL) => {
                    if store.take_cancel_request(task).await.unwrap_or(false) {
                        let _ = tx.send(true);
                        return;
                    }
                }
            }
        }
    });
    (rx, AbortOnDrop(watcher))
}

/// Whether the fix phase must run although the profile skips it.
fn fix_forced(phase: Phase, state: &RunState) -> bool {
    phase == Phase::Fix && (state.pending_validation_fix.is_some() || state.pending_human_fix)
}

/// The configured gate `phase` must pass before it runs, if any.
fn gate_before(phase: Phase, ctx: &RunContext) -> Option<ApprovalGate> {
    let gate = match phase {
        // Only when there is a spec to approve (quick profiles have none).
        Phase::Plan if ctx.spec.is_some() => ApprovalGate::Spec,
        Phase::Build => ApprovalGate::Plan,
        Phase::Merge => ApprovalGate::Merge,
        _ => return None,
    };
    ctx.config
        .pipeline
        .approvals
        .contains(&gate)
        .then_some(gate)
}

/// A phase that regenerates an approved artefact revokes its approval.
fn revoke_approvals(state: &mut RunState, phase: Phase) {
    let revoked: &[ApprovalGate] = match phase {
        Phase::Spec => &[ApprovalGate::Spec, ApprovalGate::Plan],
        Phase::Plan => &[ApprovalGate::Plan],
        Phase::Build | Phase::Fix => &[ApprovalGate::Merge],
        _ => &[],
    };
    state.approved_gates.retain(|g| !revoked.contains(g));
    if phase == Phase::Fix {
        state.pending_human_fix = false;
    }
}

/// Turn a merge rejection into a QA report for the fixer.
async fn human_review_report(ctx: &mut RunContext, reason: &str) -> Result<()> {
    ctx.state.qa_round += 1;
    let report = QaReport {
        task_id: ctx.task.id,
        round: ctx.state.qa_round,
        verdict: QaVerdict::ChangesRequested,
        summary: "Rejected by a human reviewer before merge.".into(),
        issues: vec![QaIssue {
            severity: Severity::High,
            title: "Human review".into(),
            detail: reason.to_string(),
            requirement: None,
            file: None,
            line: None,
            suggested_fix: None,
        }],
    };
    ctx.store.save_qa_report(&report).await?;
    ctx.artefact_written(vibe_core::Artefact::QaReport {
        round: report.round,
    })
    .await;
    ctx.state.pending_human_fix = true;
    ctx.store.save_run_state(&ctx.state).await
}

/// Pause the run because a budget limit was reached: the run stays
/// resumable at the current phase once the limit is raised.
async fn pause_for_budget(ctx: &mut RunContext, reason: String) {
    let reason = format!("{reason}; raise the limit and resume to continue");
    ctx.events
        .publish(Event::Paused {
            run: ctx.run_id,
            reason: reason.clone(),
        })
        .await;
    let _ = ctx.note(&format!("Run paused: {reason}.")).await;
    ctx.state.last_error = Some(reason);
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
        // One process at a time runs a task; the lock lives until the end.
        let _run_lock = store.lock_run(task.id).await?;
        let (cancel, _cancel_watch) = cancel_token(options.cancel.take(), &store, task.id);
        options.cancel = Some(cancel);

        self.router_installed
            .get_or_init(|| async {
                let router: Arc<dyn EventSink> = self.router.clone();
                events.add_sink(router).await;
            })
            .await;
        self.router
            .set(run_id, store.event_sink(task.id, run_id).await?);
        // A resumed run continues its numbering.
        let last_seq = store
            .load_events(task.id)
            .await?
            .iter()
            .filter(|e| e.event.run_id() == Some(run_id))
            .filter_map(|e| e.seq)
            .max()
            .unwrap_or(0);
        events.resume_sequence(run_id, last_seq);

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
        let base = (state.usage, state.active_ms);
        let budget = Arc::new(RunBudget::new(
            self.config.pipeline.budget_limits(),
            state.usage.total(),
            Duration::from_millis(state.active_ms),
        ));
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
            subtask_workspaces: self.deps.subtask_workspaces.clone(),
            cancel: options.cancel.clone(),
            budget,
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
            if !(ctx.profile.has(phase) || fix_forced(phase, &ctx.state)) {
                current = ctx.profile.next_after(phase);
                continue;
            }
            if let Some(exceeded) = ctx.budget.exceeded() {
                ctx.state.current_phase = phase;
                pause_for_budget(&mut ctx, exceeded.to_string()).await;
                break (TaskStatus::Backlog, RunStatus::Paused);
            }
            if ctx.is_cancelled() {
                ctx.state.current_phase = phase;
                ctx.state.last_error = Some("cancelled".into());
                break (TaskStatus::Cancelled, RunStatus::Cancelled);
            }
            if let Some(gate) = gate_before(phase, &ctx) {
                if ctx.state.rejection.as_ref().is_some_and(|r| r.gate == gate) {
                    // Send the work back to the phase that produced it.
                    let back = match gate {
                        ApprovalGate::Spec => Phase::Spec,
                        ApprovalGate::Plan => Phase::Plan,
                        ApprovalGate::Merge => {
                            let reason = ctx
                                .state
                                .rejection
                                .take()
                                .map(|r| r.comment)
                                .unwrap_or_default();
                            human_review_report(&mut ctx, &reason).await?;
                            Phase::Fix
                        }
                    };
                    let _ = ctx
                        .note(&format!(
                            "The {gate} was rejected: back to the {back} phase."
                        ))
                        .await;
                    current = Some(back);
                    continue;
                }
                if !ctx.state.approved_gates.contains(&gate) {
                    ctx.state.current_phase = phase;
                    if ctx.state.pending_approval != Some(gate) {
                        ctx.state.pending_approval = Some(gate);
                        events
                            .publish(Event::ApprovalRequested { run: run_id, gate })
                            .await;
                    }
                    let reason = format!(
                        "waiting for approval of the {gate}: `vibe approve` to continue, \
                         `vibe reject --reason \"…\"` to send it back"
                    );
                    events
                        .publish(Event::Paused {
                            run: run_id,
                            reason: reason.clone(),
                        })
                        .await;
                    let _ = ctx.note(&format!("Run paused: {reason}.")).await;
                    ctx.state.last_error = Some(reason);
                    break (TaskStatus::Review, RunStatus::Paused);
                }
            }
            ctx.phase = phase;
            ctx.state.current_phase = phase;
            ctx.state.status = RunStatus::Running;
            sync_accounting(&mut ctx, base, started);
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
            // Model calls made outside agents (assisted conflict resolution)
            // count against the run budget too.
            let budget = Arc::clone(&ctx.budget);
            let result = vibe_core::budget::scope(budget, async {
                match phase {
                    Phase::Assess => phases::run_assess(&mut ctx).await,
                    Phase::Spec => phases::run_spec(&mut ctx).await,
                    Phase::Plan => phases::run_plan(&mut ctx).await,
                    Phase::Build => phases::run_build(&mut ctx).await,
                    Phase::Qa => phases::run_qa(&mut ctx).await,
                    Phase::Fix => phases::run_fix(&mut ctx).await,
                    Phase::Merge => phases::run_merge(&mut ctx).await,
                }
            })
            .await;
            let usage = usage_delta(ctx.usage, before);
            ctx.budget_updated().await;

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
                    revoke_approvals(&mut ctx.state, phase);
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
                Err(e) if e.kind == ErrorKind::Cancelled && ctx.budget.exceeded().is_some() => {
                    let reason = ctx
                        .budget
                        .exceeded()
                        .map(|x| x.to_string())
                        .unwrap_or_default();
                    events
                        .publish(Event::PhaseFinished {
                            run: run_id,
                            phase,
                            success: false,
                            summary: reason.clone(),
                        })
                        .await;
                    ctx.registry.after_phase(phase, &ctx.task, false).await;
                    phases_run.push(PhaseResult {
                        phase,
                        success: false,
                        summary: reason.clone(),
                        usage,
                        next: Transition::Stop {
                            status: TaskStatus::Backlog,
                            reason: reason.clone(),
                            pause: true,
                        },
                    });
                    pause_for_budget(&mut ctx, reason).await;
                    Flow::End(TaskStatus::Backlog, RunStatus::Paused)
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
                        && !fix_forced(p, &ctx.state)
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
                    sync_accounting(&mut ctx, base, started);
                    ctx.state.touch();
                    store.save_run_state(&ctx.state).await?;
                    current = next;
                }
            }
        };

        ctx.task.set_status(final_status);
        store.save_task(&ctx.task).await?;
        ctx.state.status = run_status;
        sync_accounting(&mut ctx, base, started);
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
