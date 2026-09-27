//! `build`: implement the subtasks of the plan, in parallel where allowed.
//!
//! ## Scheduling
//!
//! A pending subtask is *ready* when
//!
//! * every subtask in its explicit `depends_on` is **done**, and
//! * every subtask of earlier plan phases, and — in a phase that is not
//!   `parallel` — the previous subtask of its own phase, is **finished**
//!   (done, failed or skipped).
//!
//! Up to `max_parallel_subtasks` ready subtasks run concurrently, each in a
//! fresh coder session. Only this scheduler mutates the plan; sessions just
//! return their outcome.
//!
//! ## Attempts
//!
//! A session that does not end with `{"status": "done", …}` is a failed
//! attempt. The subtask is retried until `max_subtask_attempts` attempts were
//! made (the last one uses the `coder_recovery` agent with the history of
//! failures), then marked **failed**; every pending subtask depending on it
//! (transitively, through `depends_on` only) is marked **skipped**.
//!
//! The build succeeds when at least one subtask is done and none is still
//! pending.
//!
//! ## Commits
//!
//! Completed subtasks are committed through the injected committer once no
//! other session is running (the committer stages the whole workspace, so a
//! commit taken while a sibling session edits files would capture half of
//! its work). Subtasks finishing together share one commit.
//!
//! After a failed attempt, the injected resetter discards the changes it
//! left, once no other session is running; until then no new session
//! starts, so a retry always begins from the last commit. Without a
//! resetter the changes stay in place and a progress note says so.
//!
//! ## Subtask workspaces
//!
//! With [`vibe_core::SubtaskWorkspaces`] injected and
//! `pipeline.isolate_subtasks` on (the CLI does this for the `git_worktree`
//! workspace), every attempt instead runs in its own workspace forked from
//! the current state of the task workspace, so parallel sessions never see
//! each other's half-finished edits. A successful attempt is committed and
//! integrated into the task workspace right away, one at a time: sessions
//! that finish together are integrated in plan order, and a subtask only
//! starts once the work of its dependencies is integrated. An integration
//! conflict fails the attempt, whose retry starts from the updated task
//! workspace. Failed attempts are simply thrown away: nothing waits for
//! other sessions to commit or reset. Leftover attempt workspaces are
//! removed when the build starts and ends.
//!
//! Each session runs in its own tokio task; the tasks are aborted when the
//! build ends early or its future is dropped.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use futures::FutureExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use vibe_agents::{ContinuationPolicy, run_with_continuation};
use vibe_core::{
    AgentOutcome, AgentRole, AgentStop, Error, ErrorKind, Event, Phase, Plan, Result, SubtaskId,
    SubtaskIntegration, SubtaskStatus, SubtaskWorkspaces, TaskStatus, Workspace,
};

use super::{lenient_string, lenient_strings};
use crate::context::{PhaseResult, RunContext, Transition};
use crate::kickoff::{DOCUMENT_MAX_CHARS, KickoffData, kickoff_for, truncate_head};
use crate::store::{MemoryFile, plan_to_markdown};

/// Final JSON document of a coder session.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CoderReport {
    /// `done` or `failed` (aliases: `success`, `completed`, `ok`).
    #[serde(default, deserialize_with = "lenient_string")]
    pub status: String,
    /// What was changed and verified.
    #[serde(default, deserialize_with = "lenient_string")]
    pub summary: String,
    /// Files touched.
    #[serde(default, deserialize_with = "lenient_strings")]
    pub files_changed: Vec<String>,
    /// Notes for later sessions.
    #[serde(default, deserialize_with = "lenient_string")]
    pub notes: String,
}

impl CoderReport {
    /// Whether the coder reports success.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(
            self.status.trim().to_ascii_lowercase().as_str(),
            "done" | "success" | "succeeded" | "completed" | "complete" | "ok"
        )
    }
}

fn is_finished(status: SubtaskStatus) -> bool {
    matches!(
        status,
        SubtaskStatus::Done | SubtaskStatus::Failed | SubtaskStatus::Skipped
    )
}

/// Ordering predecessors of every subtask (earlier phases, and the previous
/// subtask in a sequential phase).
fn ordering_predecessors(plan: &Plan) -> HashMap<SubtaskId, Vec<SubtaskId>> {
    let mut out = HashMap::new();
    let mut earlier: Vec<SubtaskId> = Vec::new();
    for phase in &plan.phases {
        let mut previous: Option<SubtaskId> = None;
        for s in &phase.subtasks {
            let mut preds = earlier.clone();
            if !phase.parallel
                && let Some(p) = previous
            {
                preds.push(p);
            }
            out.insert(s.id, preds);
            previous = Some(s.id);
        }
        earlier.extend(phase.subtasks.iter().map(|s| s.id));
    }
    out
}

/// Mark as skipped every pending subtask whose explicit dependency failed
/// or was skipped (transitively). Returns the newly skipped ids.
pub(crate) fn propagate_skips(plan: &mut Plan) -> Vec<SubtaskId> {
    let mut skipped = Vec::new();
    loop {
        let status: HashMap<SubtaskId, SubtaskStatus> =
            plan.subtasks().map(|s| (s.id, s.status)).collect();
        let mut changed = false;
        for s in plan.subtasks_mut() {
            if s.status == SubtaskStatus::Pending
                && s.depends_on.iter().any(|d| {
                    matches!(
                        status.get(d),
                        Some(SubtaskStatus::Failed | SubtaskStatus::Skipped)
                    )
                })
            {
                s.status = SubtaskStatus::Skipped;
                s.notes.push_str("Skipped: a dependency failed.\n");
                skipped.push(s.id);
                changed = true;
            }
        }
        if !changed {
            return skipped;
        }
    }
}

/// First ready subtask in plan order (see the module documentation).
pub(crate) fn next_ready(
    plan: &Plan,
    preds: &HashMap<SubtaskId, Vec<SubtaskId>>,
    in_flight: &HashSet<SubtaskId>,
) -> Option<SubtaskId> {
    let status: HashMap<SubtaskId, SubtaskStatus> =
        plan.subtasks().map(|s| (s.id, s.status)).collect();
    plan.subtasks()
        .find(|s| {
            s.status == SubtaskStatus::Pending
                && !in_flight.contains(&s.id)
                && s.depends_on
                    .iter()
                    .all(|d| status.get(d) == Some(&SubtaskStatus::Done))
                && preds.get(&s.id).is_none_or(|ps| {
                    ps.iter()
                        .all(|p| status.get(p).is_some_and(|st| is_finished(*st)))
                })
        })
        .map(|s| s.id)
}

fn position(plan: &Plan, id: SubtaskId) -> usize {
    plan.subtasks()
        .position(|s| s.id == id)
        .map_or(0, |i| i + 1)
}

/// Render one subtask for the coder prompt.
#[must_use]
pub fn render_subtask(plan: &Plan, id: SubtaskId) -> String {
    let Some(s) = plan.subtask(id) else {
        return String::new();
    };
    let mut out = format!(
        "### Subtask {}/{}: {}\n\n{}\n",
        position(plan, id),
        plan.len(),
        s.title,
        s.description.trim()
    );
    if !s.files.is_empty() {
        out.push_str("\nFiles:\n");
        for f in &s.files {
            out.push_str(&format!("- `{f}`\n"));
        }
    }
    if !s.verification.is_empty() {
        out.push_str("\nVerification:\n");
        for v in &s.verification {
            out.push_str(&format!("- {v}\n"));
        }
    }
    if !s.depends_on.is_empty() {
        let deps: Vec<String> = s
            .depends_on
            .iter()
            .filter_map(|d| plan.subtask(*d).map(|x| x.title.clone()))
            .collect();
        out.push_str(&format!(
            "\nBuilds on (already done): {}\n",
            deps.join(", ")
        ));
    }
    out
}

type Attempt = BoxFuture<'static, (SubtaskId, Result<AgentOutcome>)>;

/// Aborts every spawned coder session when dropped, so that sessions never
/// outlive the build (cancellation, timeout or a dropped run future).
#[derive(Default)]
struct AbortOnDrop(HashMap<SubtaskId, tokio::task::AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in self.0.values() {
            handle.abort();
        }
    }
}

/// Commit message for subtasks completed since the last commit.
fn commit_message(done: &[(usize, String)]) -> String {
    match done {
        [(n, title)] => format!("vibe: complete subtask {n} - {title}"),
        many => {
            let numbers: Vec<String> = many.iter().map(|(n, _)| n.to_string()).collect();
            let titles: Vec<&str> = many.iter().map(|(_, t)| t.as_str()).collect();
            format!(
                "vibe: complete subtasks {} - {}",
                numbers.join(", "),
                titles.join("; ")
            )
        }
    }
}

async fn publish_status(ctx: &RunContext, id: SubtaskId, status: SubtaskStatus) {
    ctx.events
        .publish(Event::SubtaskUpdated {
            run: ctx.run_id,
            subtask: id,
            status,
        })
        .await;
}

/// Start one attempt of subtask `id`: mark it in progress, persist the plan,
/// open its own workspace when subtasks are isolated, and return the
/// session future.
async fn launch(
    ctx: &mut RunContext,
    plan: &mut Plan,
    id: SubtaskId,
    max_attempts: u32,
    memory: &str,
    spec_text: &str,
    isolation: Option<&Arc<dyn SubtaskWorkspaces>>,
) -> Result<(Attempt, tokio::task::AbortHandle, Option<Workspace>)> {
    let attempt = {
        let s = plan
            .subtask_mut(id)
            .ok_or_else(|| Error::other("unknown subtask"))?;
        s.status = SubtaskStatus::InProgress;
        s.attempts += 1;
        s.attempts
    };
    ctx.store.save_plan(plan).await?;
    ctx.plan = Some(plan.clone());
    publish_status(ctx, id, SubtaskStatus::InProgress).await;

    let role = if max_attempts > 1 && attempt >= max_attempts {
        AgentRole::CoderRecovery
    } else {
        AgentRole::Coder
    };
    let agent = ctx.agent_spec(&role)?;
    let subtask_text = render_subtask(plan, id);
    let plan_text = truncate_head(&plan_to_markdown(plan), DOCUMENT_MAX_CHARS);
    let progress = ctx.progress_text().await?;
    let history = plan
        .subtask(id)
        .map(|s| s.notes.clone())
        .unwrap_or_default();
    let attempt_ws = match isolation {
        Some(iso) => {
            let label = format!("s{}-a{attempt}", position(plan, id));
            Some(iso.open(&ctx.workspace, &label).await?)
        }
        None => None,
    };
    let root = attempt_ws
        .as_ref()
        .map_or_else(|| ctx.workspace.root.clone(), |w| w.root.clone());
    let runner = ctx
        .runner_in(&agent, root)?
        .subtask(id)
        .var("spec", spec_text)
        .var("plan", plan_text.clone())
        .var("subtask", subtask_text.clone())
        .var("progress", progress.clone())
        .var("memory", memory)
        .var("prior_context", history.clone());
    let mut data = KickoffData::new(&ctx.task);
    data.spec = spec_text;
    data.plan = &plan_text;
    data.subtask = &subtask_text;
    data.progress = &progress;
    data.memory = memory;
    data.prior_context = &history;
    let message = kickoff_for(&role, &data, &agent.system_prompt);
    // Each session runs in its own task: a session parked inside
    // `FuturesUnordered` while the scheduler awaits something else could hold
    // a lock (an event sink, for example) the scheduler needs, and deadlock.
    let handle = tokio::spawn(async move {
        run_with_continuation(&runner, &agent, message, ContinuationPolicy::default()).await
    });
    let abort = handle.abort_handle();
    let fut: Attempt = Box::pin(async move {
        match handle.await {
            Ok(outcome) => (id, outcome),
            Err(e) if e.is_cancelled() => (
                id,
                Err(Error::new(ErrorKind::Cancelled, "coder session aborted")),
            ),
            Err(e) => (
                id,
                Err(Error::other(format!("coder session panicked: {e}"))),
            ),
        }
    });
    Ok((fut, abort, attempt_ws))
}

/// Integrate a successful attempt; `Some(reason)` when it could not be.
async fn integrate_attempt(
    ctx: &RunContext,
    iso: &Arc<dyn SubtaskWorkspaces>,
    id: SubtaskId,
    attempt: &Workspace,
    message: &str,
) -> Option<String> {
    let outcome = iso.integrate(&ctx.workspace, attempt, message).await;
    if let Ok(integration) = &outcome {
        let (commit, conflicts) = match integration {
            SubtaskIntegration::Integrated { commit } => (commit.clone(), Vec::new()),
            SubtaskIntegration::Conflict { files } => (None, files.clone()),
        };
        ctx.events
            .publish(Event::SubtaskIntegrated {
                run: ctx.run_id,
                subtask: id,
                commit,
                conflicts,
            })
            .await;
    }
    match outcome {
        Ok(SubtaskIntegration::Integrated { .. }) => None,
        Ok(SubtaskIntegration::Conflict { files }) => Some(format!(
            "its changes conflict with work integrated since the attempt started ({}); \
             the next attempt starts from the updated task workspace",
            files.join(", ")
        )),
        Err(e) => Some(format!("integration failed: {e}")),
    }
}

/// Remove an attempt workspace; failures are logged, never fatal.
async fn discard_attempt(
    ctx: &RunContext,
    iso: Option<&Arc<dyn SubtaskWorkspaces>>,
    attempt: Option<Workspace>,
) {
    let (Some(iso), Some(ws)) = (iso, attempt) else {
        return;
    };
    if let Err(e) = iso.discard(&ws).await {
        ctx.events
            .log(
                Some(ctx.run_id),
                "warn",
                format!("cannot remove subtask workspace {}: {e}", ws.root.display()),
            )
            .await;
    }
}

/// Remove every attempt workspace of the task; failures are logged.
async fn discard_all_attempts(ctx: &RunContext, iso: Option<&Arc<dyn SubtaskWorkspaces>>) {
    let Some(iso) = iso else {
        return;
    };
    if let Err(e) = iso.discard_all(&ctx.workspace).await {
        ctx.events
            .log(
                Some(ctx.run_id),
                "warn",
                format!("cannot remove subtask workspaces: {e}"),
            )
            .await;
    }
}

/// Put an interrupted attempt back to pending, without counting it.
fn requeue(plan: &mut Plan, id: SubtaskId) {
    if let Some(s) = plan.subtask_mut(id) {
        s.status = SubtaskStatus::Pending;
        s.attempts = s.attempts.saturating_sub(1);
    }
}

/// Why an attempt failed, or `None` when it succeeded.
fn failure_reason(outcome: &AgentOutcome) -> (Option<String>, CoderReport) {
    if let AgentStop::Error { message, .. } = &outcome.stop {
        return (
            Some(format!("session error: {message}")),
            CoderReport::default(),
        );
    }
    match vibe_agents::parse_structured::<CoderReport>(&outcome.final_text) {
        Ok(r) if r.is_done() => (None, r),
        Ok(r) => {
            let mut why = format!("coder reported `{}`", r.status.trim());
            for extra in [&r.summary, &r.notes] {
                if !extra.trim().is_empty() {
                    why.push_str(": ");
                    why.push_str(extra.trim());
                }
            }
            (Some(why), r)
        }
        Err(e) => (
            Some(format!(
                "no valid status report (session stopped: {:?}): {}",
                outcome.stop, e.message
            )),
            CoderReport::default(),
        ),
    }
}

/// Run the build scheduler (see the module documentation).
pub async fn run_build(ctx: &mut RunContext) -> Result<PhaseResult> {
    let mut plan = match ctx.plan.clone() {
        Some(p) => p,
        None => ctx
            .store
            .load_plan(ctx.task.id)
            .await?
            .ok_or_else(|| Error::other("no plan to build: run the plan phase first"))?,
    };
    // Subtasks interrupted by a crash are retried.
    for s in plan.subtasks_mut() {
        if s.status == SubtaskStatus::InProgress {
            s.status = SubtaskStatus::Pending;
        }
    }
    if plan.is_empty() {
        return Ok(PhaseResult::ok(Phase::Build, "the plan has no subtask")
            .with_success(false)
            .then(Transition::Stop {
                status: TaskStatus::Failed,
                reason: "the plan has no subtask".into(),
                pause: false,
            }));
    }
    let preds = ordering_predecessors(&plan);
    let max_parallel = ctx.config.pipeline.max_parallel_subtasks.max(1);
    let max_attempts = ctx.config.pipeline.max_subtask_attempts.max(1);
    let memory = ctx.memory_text().await?;
    let spec_text = ctx.spec_text();

    let mut running: FuturesUnordered<Attempt> = FuturesUnordered::new();
    let mut in_flight: HashSet<SubtaskId> = HashSet::new();
    let mut aborts = AbortOnDrop::default();
    // Subtasks done but not committed yet: the committer stages the whole
    // workspace, so commits wait until no other session is editing it.
    let mut to_commit: Vec<(usize, String)> = Vec::new();
    // Failed attempts whose leftovers must be discarded once no session is
    // editing the workspace. While any is pending, no session starts, so a
    // retry never begins from a dirty tree.
    let mut to_reset: Vec<String> = Vec::new();
    let mut cancelled = false;
    let isolation: Option<Arc<dyn SubtaskWorkspaces>> = ctx
        .subtask_workspaces
        .clone()
        .filter(|_| ctx.config.pipeline.isolate_subtasks);
    let isolated = isolation.is_some();
    let mut attempt_workspaces: HashMap<SubtaskId, Workspace> = HashMap::new();
    if isolated {
        // Attempts fork from the last commit: record what earlier phases or
        // a human left, and drop attempt workspaces of a crashed process.
        ctx.commit("vibe: checkpoint before build").await;
        discard_all_attempts(ctx, isolation.as_ref()).await;
    }

    loop {
        if ctx.is_cancelled() {
            cancelled = true;
            break;
        }
        let skipped = propagate_skips(&mut plan);
        if !skipped.is_empty() {
            ctx.store.save_plan(&plan).await?;
            for id in skipped {
                publish_status(ctx, id, SubtaskStatus::Skipped).await;
                let title = plan
                    .subtask(id)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                ctx.note(&format!("Subtask `{title}` skipped: a dependency failed."))
                    .await?;
            }
        }
        while in_flight.len() < max_parallel && to_reset.is_empty() {
            let Some(id) = next_ready(&plan, &preds, &in_flight) else {
                break;
            };
            let (fut, abort, attempt_ws) = launch(
                ctx,
                &mut plan,
                id,
                max_attempts,
                &memory,
                &spec_text,
                isolation.as_ref(),
            )
            .await?;
            if let Some(ws) = attempt_ws {
                attempt_workspaces.insert(id, ws);
            }
            aborts.0.insert(id, abort);
            in_flight.insert(id);
            running.push(fut);
        }
        if in_flight.is_empty() {
            // Nothing runs and nothing is ready: whatever is pending is
            // blocked (for example by an ill-ordered dependency).
            let blocked: Vec<SubtaskId> = plan
                .subtasks()
                .filter(|s| s.status == SubtaskStatus::Pending)
                .map(|s| s.id)
                .collect();
            if blocked.is_empty() {
                break;
            }
            for id in &blocked {
                if let Some(s) = plan.subtask_mut(*id) {
                    s.status = SubtaskStatus::Skipped;
                    s.notes.push_str("Skipped: blocked by the plan ordering.\n");
                }
                publish_status(ctx, *id, SubtaskStatus::Skipped).await;
            }
            ctx.store.save_plan(&plan).await?;
            continue;
        }

        let Some(first) = running.next().await else {
            break;
        };
        // Sessions that finished together are handled in plan order, so the
        // order of integration never depends on scheduling luck.
        let mut batch = vec![first];
        while let Some(Some(next)) = running.next().now_or_never() {
            batch.push(next);
        }
        batch.sort_by_key(|(id, _)| position(&plan, *id));
        for (id, result) in batch {
            in_flight.remove(&id);
            aborts.0.remove(&id);
            let attempt_ws = attempt_workspaces.remove(&id);
            let outcome = match result {
                Ok(o) => {
                    ctx.record(&o);
                    if o.stop == AgentStop::Cancelled {
                        requeue(&mut plan, id);
                        discard_attempt(ctx, isolation.as_ref(), attempt_ws).await;
                        cancelled = true;
                        continue;
                    }
                    Some(o)
                }
                Err(e) if e.kind == ErrorKind::Cancelled => {
                    requeue(&mut plan, id);
                    discard_attempt(ctx, isolation.as_ref(), attempt_ws).await;
                    cancelled = true;
                    continue;
                }
                Err(e) => {
                    ctx.events
                        .log(
                            Some(ctx.run_id),
                            "warn",
                            format!("coder session failed: {e}"),
                        )
                        .await;
                    None
                }
            };
            let (mut reason, report) = match &outcome {
                Some(o) => failure_reason(o),
                None => (
                    Some("the session could not run".into()),
                    CoderReport::default(),
                ),
            };
            let n = position(&plan, id);
            let title = plan
                .subtask(id)
                .map(|s| s.title.clone())
                .unwrap_or_default();
            if reason.is_none()
                && let (Some(iso), Some(ws)) = (isolation.as_ref(), attempt_ws.as_ref())
            {
                let message = commit_message(&[(n, title.clone())]);
                reason = integrate_attempt(ctx, iso, id, ws, &message).await;
            }
            discard_attempt(ctx, isolation.as_ref(), attempt_ws).await;
            ctx.budget_updated().await;
            let Some(sub) = plan.subtask_mut(id) else {
                continue;
            };
            let attempt = sub.attempts;
            match reason {
                None => {
                    sub.status = SubtaskStatus::Done;
                    sub.notes = report.notes.trim().to_string();
                    ctx.store.save_plan(&plan).await?;
                    publish_status(ctx, id, SubtaskStatus::Done).await;
                    let files = if report.files_changed.is_empty() {
                        String::new()
                    } else {
                        format!("\n\nFiles: {}", report.files_changed.join(", "))
                    };
                    let notes = if report.notes.trim().is_empty() {
                        String::new()
                    } else {
                        format!("\n\nNotes: {}", report.notes.trim())
                    };
                    ctx.note(&format!(
                        "Subtask {n} `{title}` done (attempt {attempt}): {}{files}{notes}",
                        report.summary.trim()
                    ))
                    .await?;
                    if !isolated {
                        to_commit.push((n, title));
                    }
                }
                Some(why) => {
                    sub.notes
                        .push_str(&format!("Attempt {attempt} failed: {why}\n"));
                    let what = format!("attempt {attempt} of subtask {n} `{title}`");
                    let give_up = attempt >= max_attempts;
                    sub.status = if give_up {
                        SubtaskStatus::Failed
                    } else {
                        SubtaskStatus::Pending
                    };
                    ctx.store.save_plan(&plan).await?;
                    if isolated {
                        // The attempt workspace is gone with its changes.
                    } else if ctx.resetter.is_some() {
                        to_reset.push(what);
                    } else {
                        ctx.reset_workspace(&what).await;
                    }
                    if give_up {
                        publish_status(ctx, id, SubtaskStatus::Failed).await;
                        ctx.note(&format!(
                            "Subtask {n} `{title}` FAILED after {attempt} attempt(s): {why}"
                        ))
                        .await?;
                        ctx.remember(
                            MemoryFile::Gotchas,
                            &format!("Subtask `{title}` failed repeatedly: {why}"),
                        )
                        .await?;
                    } else {
                        publish_status(ctx, id, SubtaskStatus::Pending).await;
                        ctx.note(&format!(
                            "Subtask {n} `{title}` attempt {attempt} failed: {why}"
                        ))
                        .await?;
                        ctx.events
                            .publish(Event::Retrying {
                                run: ctx.run_id,
                                what: format!("subtask `{title}`"),
                                attempt: attempt + 1,
                                delay_ms: 0,
                            })
                            .await;
                    }
                }
            }
            ctx.plan = Some(plan.clone());
        }
        if cancelled {
            break;
        }
        if in_flight.is_empty() {
            // Completed work is committed first so that the reset below can
            // never discard it; the reset then removes whatever the failed
            // attempts left behind.
            if !to_commit.is_empty() {
                ctx.commit(&commit_message(&to_commit)).await;
                to_commit.clear();
            }
            if !to_reset.is_empty() {
                ctx.reset_workspace(&to_reset.join(", ")).await;
                to_reset.clear();
            }
        }
    }

    if cancelled {
        drop(aborts);
        drop(running);
        for id in in_flight {
            requeue(&mut plan, id);
        }
        discard_all_attempts(ctx, isolation.as_ref()).await;
        ctx.store.save_plan(&plan).await?;
        ctx.plan = Some(plan);
        return Err(Error::new(ErrorKind::Cancelled, "build cancelled"));
    }

    discard_all_attempts(ctx, isolation.as_ref()).await;
    ctx.store.save_plan(&plan).await?;
    let count = |st: SubtaskStatus| plan.subtasks().filter(|s| s.status == st).count();
    let (done, failed, skipped, pending) = (
        count(SubtaskStatus::Done),
        count(SubtaskStatus::Failed),
        count(SubtaskStatus::Skipped),
        count(SubtaskStatus::Pending) + count(SubtaskStatus::InProgress),
    );
    ctx.plan = Some(plan);
    let summary = format!("{done} done, {failed} failed, {skipped} skipped");
    ctx.note(&format!("Build finished: {summary}.")).await?;
    if done > 0 && pending == 0 {
        Ok(PhaseResult::ok(Phase::Build, summary).with_success(failed == 0 && skipped == 0))
    } else {
        Ok(PhaseResult::ok(Phase::Build, summary.clone())
            .with_success(false)
            .then(Transition::Stop {
                status: TaskStatus::Failed,
                reason: format!("build failed: {summary}"),
                pause: false,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::{PlanPhase, Subtask, TaskId};

    fn plan(phases: Vec<(bool, Vec<Subtask>)>) -> Plan {
        Plan {
            task_id: TaskId::new(),
            approach: String::new(),
            phases: phases
                .into_iter()
                .map(|(parallel, subtasks)| PlanPhase {
                    name: "p".into(),
                    parallel,
                    subtasks,
                })
                .collect(),
        }
    }

    #[test]
    fn explicit_dependencies_require_done() {
        let a = Subtask::new("a", "");
        let b = Subtask::new("b", "");
        let mut c = Subtask::new("c", "");
        c.depends_on = vec![a.id, b.id];
        let (ia, ib, ic) = (a.id, b.id, c.id);
        let mut p = plan(vec![(true, vec![a, b, c])]);
        let preds = ordering_predecessors(&p);
        let mut fl = HashSet::new();
        assert_eq!(next_ready(&p, &preds, &fl), Some(ia));
        fl.insert(ia);
        assert_eq!(next_ready(&p, &preds, &fl), Some(ib));
        fl.insert(ib);
        assert_eq!(next_ready(&p, &preds, &fl), None);
        p.subtask_mut(ia).unwrap().status = SubtaskStatus::Done;
        p.subtask_mut(ib).unwrap().status = SubtaskStatus::Failed;
        fl.clear();
        assert_eq!(next_ready(&p, &preds, &fl), None);
        assert_eq!(propagate_skips(&mut p), vec![ic]);
        assert_eq!(p.subtask(ic).unwrap().status, SubtaskStatus::Skipped);
    }

    #[test]
    fn phases_are_barriers_and_sequential_phases_are_ordered() {
        let a = Subtask::new("a", "");
        let b = Subtask::new("b", "");
        let c = Subtask::new("c", "");
        let (ia, ib, ic) = (a.id, b.id, c.id);
        let mut p = plan(vec![(false, vec![a, b]), (true, vec![c])]);
        let preds = ordering_predecessors(&p);
        let none = HashSet::new();
        let mut fl = HashSet::new();
        fl.insert(ia);
        // b waits for a (sequential phase), c waits for the whole first phase.
        assert_eq!(next_ready(&p, &preds, &fl), None);
        p.subtask_mut(ia).unwrap().status = SubtaskStatus::Failed;
        assert_eq!(next_ready(&p, &preds, &none), Some(ib));
        p.subtask_mut(ib).unwrap().status = SubtaskStatus::Done;
        // A failure without explicit dependency does not skip later phases.
        assert!(propagate_skips(&mut p).is_empty());
        assert_eq!(next_ready(&p, &preds, &none), Some(ic));
    }

    #[test]
    fn transitive_skips() {
        let mut a = Subtask::new("a", "");
        a.status = SubtaskStatus::Failed;
        let mut b = Subtask::new("b", "");
        b.depends_on.push(a.id);
        let mut c = Subtask::new("c", "");
        c.depends_on.push(b.id);
        let mut p = plan(vec![(true, vec![c, b, a])]);
        assert_eq!(propagate_skips(&mut p).len(), 2);
    }

    #[test]
    fn commit_messages() {
        assert_eq!(
            commit_message(&[(1, "A".into())]),
            "vibe: complete subtask 1 - A"
        );
        assert_eq!(
            commit_message(&[(1, "A".into()), (2, "B".into())]),
            "vibe: complete subtasks 1, 2 - A; B"
        );
    }

    #[test]
    fn coder_report_statuses() {
        let r: CoderReport =
            serde_json::from_str(r#"{"status":"Completed","files_changed":"a.rs"}"#).unwrap();
        assert!(r.is_done());
        assert_eq!(r.files_changed, vec!["a.rs"]);
        let r: CoderReport = serde_json::from_str(r#"{"status":"failed"}"#).unwrap();
        assert!(!r.is_done());
    }

    #[test]
    fn subtask_rendering() {
        let mut a = Subtask::new("First", "do it");
        a.files.push("x.rs".into());
        a.verification.push("cargo test".into());
        let mut b = Subtask::new("Second", "then");
        b.depends_on.push(a.id);
        let ib = b.id;
        let p = plan(vec![(true, vec![a, b])]);
        let text = render_subtask(&p, ib);
        assert!(text.starts_with("### Subtask 2/2: Second"));
        assert!(text.contains("Builds on (already done): First"));
        assert_eq!(render_subtask(&p, SubtaskId::new()), "");
    }
}
