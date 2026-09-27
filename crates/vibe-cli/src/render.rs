//! Live rendering of pipeline events and the end-of-run summary.
//!
//! The [`Renderer`] is registered as an [`EventSink`] on the bus: sinks are
//! awaited on every publish, so lines appear in event order and the last
//! event is printed before `Pipeline::run` returns (a broadcast subscriber
//! could lag or still be printing when the summary starts).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vibe_core::{Envelope, Event, EventSink, Phase, Plan, SubtaskId, TaskId, TaskStore};
use vibe_pipeline::RunReport;

use crate::util::{
    Table, enum_name, human_duration, human_tokens, short_sha, status_name, style, truncate,
};

/// Maximum characters of tool arguments shown on a tool call line.
pub const TOOL_ARGS_MAX: usize = 80;

#[derive(Default)]
struct State {
    phase_started: HashMap<Phase, Instant>,
    phase_durations: Vec<(Phase, Duration)>,
    subtasks: HashMap<SubtaskId, (usize, usize, String)>,
}

/// Prints pipeline events as they happen.
pub struct Renderer {
    json: bool,
    verbose: u8,
    task: TaskId,
    store: Arc<dyn TaskStore>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("json", &self.json)
            .field("task", &self.task)
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// Renderer for the events of `task`.
    pub fn new(json: bool, verbose: u8, task: TaskId, store: Arc<dyn TaskStore>) -> Self {
        Self {
            json,
            verbose,
            task,
            store,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Duration of each phase, in execution order.
    pub fn phase_durations(&self) -> Vec<(Phase, Duration)> {
        self.state().phase_durations.clone()
    }

    async fn subtask_label(&self, id: SubtaskId) -> String {
        if let Some((pos, total, title)) = self.state().subtasks.get(&id).cloned() {
            return format!("{pos}/{total} {title}");
        }
        if let Ok(Some(plan)) = self.store.load_plan(self.task).await {
            self.remember_plan(&plan);
        }
        match self.state().subtasks.get(&id) {
            Some((pos, total, title)) => format!("{pos}/{total} {title}"),
            None => id.short(),
        }
    }

    fn remember_plan(&self, plan: &Plan) {
        let total = plan.len();
        let mut st = self.state();
        for (i, s) in plan.subtasks().enumerate() {
            st.subtasks.insert(s.id, (i + 1, total, s.title.clone()));
        }
    }

    /// Text of one event, or `None` when it is not shown at this verbosity.
    async fn line(&self, event: &Event) -> Option<String> {
        Some(match event {
            Event::RunStarted { run, .. } => {
                format!("{} run {}", style::accent().apply_to("▶"), run.short())
            }
            Event::PhaseStarted { phase, .. } => {
                self.state().phase_started.insert(*phase, Instant::now());
                format!(
                    "\n{}",
                    style::accent().apply_to(format!("── {phase} ──────────"))
                )
            }
            Event::PhaseFinished {
                phase,
                success,
                summary,
                ..
            } => {
                let elapsed = {
                    let mut st = self.state();
                    let d = st
                        .phase_started
                        .remove(phase)
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    st.phase_durations.push((*phase, d));
                    d
                };
                let mark = if *success {
                    style::ok().apply_to("✓")
                } else {
                    style::err().apply_to("✗")
                };
                format!(
                    "{mark} {phase}: {} {}",
                    truncate(summary, 160),
                    style::dim().apply_to(format!("({})", human_duration(elapsed)))
                )
            }
            Event::AgentStarted { role, subtask, .. } => {
                let target = match subtask {
                    Some(id) => format!(" · subtask {}", self.subtask_label(*id).await),
                    None => String::new(),
                };
                format!("  {} {role}{target}", style::bold().apply_to("●"))
            }
            // Streamed text is for live interfaces and `--json`; the complete
            // text follows as `AgentText`.
            Event::AgentDelta { .. } => return None,
            Event::AgentText { text, .. } => {
                if self.verbose == 0 || text.trim().is_empty() {
                    return None;
                }
                format!("    {}", style::dim().apply_to(truncate(text.trim(), 120)))
            }
            Event::ToolCalled { tool, input, .. } => {
                format!("  ⟶ {tool}({})", format_args_short(input))
            }
            Event::ToolReturned {
                is_error,
                duration_ms,
                preview,
                ..
            } => {
                let status = if *is_error {
                    style::err().apply_to("error").to_string()
                } else {
                    style::ok().apply_to("ok").to_string()
                };
                let detail = if *is_error && !preview.trim().is_empty() {
                    format!(": {}", truncate(preview.trim(), 100))
                } else {
                    String::new()
                };
                format!("  ⟵ {status} ({duration_ms} ms){detail}")
            }
            Event::AgentFinished {
                role,
                steps,
                usage,
                stop,
                ..
            } => style::dim()
                .apply_to(format!(
                    "  ● {role} finished: {steps} step(s), {} in / {} out tokens, {}",
                    human_tokens(usage.input_tokens),
                    human_tokens(usage.output_tokens),
                    stop_text(stop)
                ))
                .to_string(),
            Event::SubtaskUpdated {
                subtask, status, ..
            } => {
                let label = self.subtask_label(*subtask).await;
                format!("  ▸ subtask {label}: {}", enum_name(status))
            }
            Event::Retrying {
                what,
                attempt,
                delay_ms,
                ..
            } => style::warn()
                .apply_to(format!(
                    "  ↻ retrying {what} (attempt {attempt}, in {})",
                    human_duration(Duration::from_millis(*delay_ms))
                ))
                .to_string(),
            Event::ValidationFinished {
                command,
                integration,
                passed,
                ..
            } => {
                let target = if *integration { " (integration)" } else { "" };
                if *passed {
                    style::ok()
                        .apply_to(format!("  ✓ validation{target}: {command}"))
                        .to_string()
                } else {
                    style::warn()
                        .apply_to(format!("  ✗ validation failed{target}: {command}"))
                        .to_string()
                }
            }
            Event::SubtaskIntegrated {
                subtask, conflicts, ..
            } => {
                let label = self.subtask_label(*subtask).await;
                if conflicts.is_empty() {
                    if self.verbose == 0 {
                        return None;
                    }
                    format!("  ▸ subtask {label}: integrated")
                } else {
                    style::warn()
                        .apply_to(format!(
                            "  ▸ subtask {label}: conflicts in {}, retrying from the updated branch",
                            conflicts.join(", ")
                        ))
                        .to_string()
                }
            }
            Event::Committed {
                commit,
                message,
                files,
                ..
            } => {
                if self.verbose == 0 {
                    return None;
                }
                style::dim()
                    .apply_to(format!(
                        "  · committed {} ({} file(s)): {}",
                        short_sha(commit),
                        files.len(),
                        truncate(message, 100)
                    ))
                    .to_string()
            }
            Event::Merged {
                commit,
                branch,
                base,
                ..
            } => style::ok()
                .apply_to(format!(
                    "  ✓ merged {branch} into {base} ({})",
                    short_sha(commit)
                ))
                .to_string(),
            Event::BudgetUpdated { .. } => return None,
            Event::ApprovalRequested { gate, .. } => style::warn()
                .apply_to(format!("⏸ approval needed: the {gate}"))
                .to_string(),
            Event::ApprovalResolved {
                gate,
                approved,
                comment,
                ..
            } => {
                let verb = if *approved { "approved" } else { "rejected" };
                let note = if comment.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", truncate(comment.trim(), 100))
                };
                format!("  ✓ {gate} {verb}{note}")
            }
            Event::ArtefactWritten { artefact, .. } => {
                if self.verbose == 0 {
                    return None;
                }
                let name = match artefact {
                    vibe_core::Artefact::Spec => "spec".to_string(),
                    vibe_core::Artefact::Plan => "plan".to_string(),
                    vibe_core::Artefact::QaReport { round } => format!("QA report {round}"),
                };
                style::dim()
                    .apply_to(format!("  · wrote the {name}"))
                    .to_string()
            }
            Event::Paused { reason, .. } => style::warn()
                .apply_to(format!("⏸ paused: {reason}"))
                .to_string(),
            Event::RunFinished {
                success, status, ..
            } => {
                let text = format!("■ run finished: {}", status_name(*status));
                if *success {
                    style::ok().apply_to(text).to_string()
                } else {
                    style::warn().apply_to(text).to_string()
                }
            }
            Event::Log { level, message, .. } => match level.as_str() {
                "error" => style::err().apply_to(format!("  ! {message}")).to_string(),
                "warn" => style::warn().apply_to(format!("  ! {message}")).to_string(),
                _ if self.verbose > 0 => {
                    style::dim().apply_to(format!("  · {message}")).to_string()
                }
                _ => return None,
            },
        })
    }
}

#[async_trait::async_trait]
impl EventSink for Renderer {
    async fn on_event(&self, envelope: &Envelope) {
        if self.json {
            match serde_json::to_string(envelope) {
                Ok(line) => println!("{line}"),
                Err(e) => tracing::warn!("cannot serialise event: {e}"),
            }
            return;
        }
        if let Some(line) = self.line(&envelope.event).await {
            println!("{line}");
        }
    }
}

/// Readable form of a serialised `AgentStop` (`completed`, `error: …`).
pub fn stop_text(stop: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(stop) else {
        return truncate(stop, 100);
    };
    let reason = map
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("stopped");
    match map.get("message").and_then(serde_json::Value::as_str) {
        Some(m) => format!("{reason}: {}", truncate(m, 100)),
        None => reason.to_string(),
    }
}

/// Compact one-line rendering of tool arguments, at most
/// [`TOOL_ARGS_MAX`] characters.
pub fn format_args_short(input: &serde_json::Value) -> String {
    let text = match input {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => format!("{k}: {s:?}"),
                other => format!("{k}: {other}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    truncate(&text, TOOL_ARGS_MAX)
}

/// Extra facts shown in the summary.
#[derive(Debug, Clone, Default)]
pub struct SummaryInfo {
    /// Task directory.
    pub task_dir: Option<PathBuf>,
    /// Branch of the workspace, if any.
    pub branch: Option<String>,
    /// Worktree directory, if any.
    pub worktree: Option<PathBuf>,
}

/// Human summary of a run.
pub fn summary_text(
    report: &RunReport,
    durations: &[(Phase, Duration)],
    info: &SummaryInfo,
) -> String {
    let mut table = Table::new([
        "phase",
        "result",
        "time",
        "tokens in",
        "tokens out",
        "summary",
    ]);
    let mut durations = durations.iter();
    for p in &report.phases {
        let time = durations
            .find(|(ph, _)| *ph == p.phase)
            .map(|(_, d)| human_duration(*d))
            .unwrap_or_default();
        let mark = if p.success {
            style::ok().apply_to("✓").to_string()
        } else {
            style::err().apply_to("✗").to_string()
        };
        table.row([
            p.phase.to_string(),
            mark,
            time,
            human_tokens(p.usage.input_tokens),
            human_tokens(p.usage.output_tokens),
            truncate(&p.summary, 60),
        ]);
    }
    let mut out = String::from("\n");
    if !table.is_empty() {
        out.push_str(&table.render());
        out.push('\n');
    }
    let status = crate::util::styled_status(report.final_status);
    out.push_str(&format!("status     {status}\n"));
    out.push_str(&format!("duration   {}\n", human_duration(report.duration)));
    out.push_str(&format!(
        "tokens     {} in / {} out\n",
        human_tokens(report.usage.input_tokens),
        human_tokens(report.usage.output_tokens)
    ));
    if report.state.usage != report.usage {
        // Resumed run: show what the whole run consumed so far.
        out.push_str(&format!(
            "run total  {} in / {} out, {} active\n",
            human_tokens(report.state.usage.input_tokens),
            human_tokens(report.state.usage.output_tokens),
            human_duration(std::time::Duration::from_millis(report.state.active_ms))
        ));
    }
    if let Some(err) = &report.state.last_error {
        out.push_str(&format!("note       {}\n", truncate(err, 200)));
    }
    if let Some(b) = &info.branch {
        out.push_str(&format!("branch     {b}\n"));
    }
    if let Some(w) = &info.worktree {
        out.push_str(&format!("worktree   {}\n", w.display()));
    }
    if let Some(d) = &info.task_dir {
        out.push_str(&format!("task dir   {}\n", d.display()));
    }
    out
}

/// Machine-readable summary of a run.
pub fn summary_json(report: &RunReport, info: &SummaryInfo, exit_code: u8) -> serde_json::Value {
    serde_json::json!({
        "type": "summary",
        "run_id": report.run_id,
        "task_id": report.task.id,
        "final_status": report.final_status,
        "run_status": report.state.status,
        "success": report.is_success(),
        "exit_code": exit_code,
        "duration_ms": report.duration.as_millis() as u64,
        "usage": report.usage,
        "run_usage": report.state.usage,
        "run_active_ms": report.state.active_ms,
        "phases": report.phases,
        "last_error": report.state.last_error,
        "branch": info.branch,
        "worktree": info.worktree,
        "task_dir": info.task_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_args_are_short() {
        assert_eq!(
            format_args_short(&json!({"path": "src/lib.rs"})),
            "path: \"src/lib.rs\""
        );
        let long = format_args_short(&json!({"content": "x".repeat(500)}));
        assert_eq!(long.chars().count(), TOOL_ARGS_MAX);
        assert!(long.ends_with('…'));
        assert_eq!(format_args_short(&serde_json::Value::Null), "");
    }

    #[test]
    fn stop_reasons_are_readable() {
        assert_eq!(stop_text(r#"{"reason":"completed"}"#), "completed");
        assert_eq!(
            stop_text(r#"{"reason":"error","kind":"other","message":"boom"}"#),
            "error: boom"
        );
        assert_eq!(stop_text("max_steps"), "max_steps");
    }
}
