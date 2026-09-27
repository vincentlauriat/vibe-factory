//! `vibe history`: what finished tasks did — runs, commits, changed files,
//! tokens, active time and cost.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use vibe_core::{SubtaskId, Task, VibeConfig};
use vibe_pipeline::history::{Cost, FileStatus, RunSummary, RunSummaryState, TaskHistory};
use vibe_pipeline::{
    ChangedFilesSource, HistoryFilter, PipelineStore, project_history, task_history,
};

use super::{resolve_task, subtask_labels};
use crate::app::{self, load_config, open_store, worktree_location};
use crate::cli::HistoryArgs;
use crate::util::{
    Table, Ui, enum_name, human_duration, human_tokens, print_out, relative_time, short_sha, style,
    styled_status, truncate,
};

/// Run `vibe history`.
pub async fn run(root: &Path, args: HistoryArgs, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    let config = load_config(root)?;
    let branch_of = |task: &Task| task_branch(root, &config, task);
    if let Some(reference) = &args.reference {
        let task = resolve_task(&store, reference).await?;
        let branch = branch_of(&task);
        let history = task_history(&*store, root, &config, task, branch.as_deref()).await?;
        if ui.json {
            ui.print_json(&serde_json::to_value(&history)?);
            return Ok(0);
        }
        let events = store.load_events(history.task.id).await?;
        let subtasks = subtask_labels(&store, &history.task).await;
        // The derived branch is only a guess: show it once git confirmed it.
        let shown = history
            .task
            .branch
            .clone()
            .or_else(|| match &history.changed_files.source {
                ChangedFilesSource::Branch { branch, .. } => Some(branch.clone()),
                _ => None,
            });
        let text = detail(&history, shown.as_deref(), &events, &config, &subtasks);
        print_out(&text)?;
        return Ok(0);
    }

    let filter = HistoryFilter { all: args.all };
    let histories = project_history(&*store, root, &config, filter, &branch_of).await?;
    if ui.json {
        ui.print_json(&serde_json::to_value(&histories)?);
        return Ok(0);
    }
    for h in &histories {
        for e in &h.errors {
            eprintln!("{} #{}: {e}", style::warn().apply_to("warning:"), h.number);
        }
    }
    if histories.is_empty() {
        let hint = if args.all {
            ""
        } else {
            " (failed and cancelled ones are shown with --all)"
        };
        println!("No finished tasks yet{hint}.");
        return Ok(0);
    }
    print_out(&table(&histories))?;
    Ok(0)
}

/// Branch of a task for the history: the recorded one, else the one the
/// worktree provider derives (tasks run before 0.5). Other workspace
/// providers have no branch to compare.
fn task_branch(root: &Path, config: &VibeConfig, task: &Task) -> Option<String> {
    task.branch.clone().or_else(|| {
        app::uses_worktrees(&config.pipeline.workspace)
            .then(|| worktree_location(root, task).branch)
    })
}

/// The project table.
fn table(histories: &[TaskHistory]) -> String {
    let mut table = Table::new([
        "#", "title", "status", "runs", "commits", "files", "tokens", "active", "cost", "finished",
    ]);
    for h in histories {
        let finished = h
            .runs
            .iter()
            .rev()
            .find_map(|r| r.finished_at)
            .unwrap_or(h.last_activity);
        let files = format!(
            "{}{}",
            h.changed_files.files.len(),
            if h.changed_files.approximate { "~" } else { "" }
        );
        table.row([
            h.number.to_string(),
            truncate(&h.task.title, 40),
            styled_status(h.task.status),
            h.totals.runs.to_string(),
            h.totals.commits.to_string(),
            files,
            lower_bound(
                human_tokens(h.totals.usage.input_tokens + h.totals.usage.output_tokens),
                h.totals.complete,
            ),
            lower_bound(
                human_duration(Duration::from_millis(h.totals.active_ms)),
                h.totals.complete,
            ),
            h.cost.as_ref().map_or_else(|| "-".to_string(), cost_text),
            style::dim().apply_to(relative_time(finished)).to_string(),
        ]);
    }
    let mut out = table.render();
    let partial = histories
        .iter()
        .any(|h| !h.totals.complete || h.cost.as_ref().is_some_and(|c| !c.complete));
    let approximate = histories.iter().any(|h| h.changed_files.approximate);
    if partial || approximate {
        let mut notes = Vec::new();
        if partial {
            notes.push("+ lower bound (runs logged before 0.5 or unfinished, tokens not priced)");
        }
        if approximate {
            notes.push("~ approximate file count");
        }
        out.push_str(&style::dim().apply_to(notes.join("; ")).to_string());
        out.push('\n');
    }
    out
}

/// `value`, marked `+` when it is only a lower bound.
fn lower_bound(value: String, complete: bool) -> String {
    if complete { value } else { format!("{value}+") }
}

/// `0.42 USD`, marked `+` when some tokens could not be priced.
fn cost_text(cost: &Cost) -> String {
    lower_bound(
        format!("{:.2} {}", cost.amount, cost.currency),
        cost.complete,
    )
}

fn run_state(run: &RunSummary) -> String {
    match run.state {
        RunSummaryState::Running => style::warn().apply_to("running").to_string(),
        RunSummaryState::Finished => style::ok().apply_to("finished").to_string(),
        RunSummaryState::Interrupted => style::err().apply_to("interrupted").to_string(),
    }
}

fn status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Copied => "C",
        FileStatus::Unknown => "?",
    }
}

fn source_text(source: &ChangedFilesSource) -> String {
    match source {
        ChangedFilesSource::Branch { branch, base } => format!("diff of {branch} against {base}"),
        ChangedFilesSource::MergeCommit {
            commit,
            fast_forward,
        } => format!(
            "merge {}{}",
            short_sha(commit),
            if *fast_forward { " (fast-forward)" } else { "" }
        ),
        ChangedFilesSource::Commits => "commit events".to_string(),
        ChangedFilesSource::Trace => "files written by the agents".to_string(),
        ChangedFilesSource::None => "nothing recorded".to_string(),
    }
}

/// The detail of one task.
fn detail(
    h: &TaskHistory,
    branch: Option<&str>,
    events: &[vibe_core::Envelope],
    config: &VibeConfig,
    subtasks: &HashMap<SubtaskId, String>,
) -> String {
    use std::fmt::Write;
    let head = |s: &str| style::accent().apply_to(s).to_string();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        style::bold().apply_to(format!("#{}", h.number)),
        style::bold().apply_to(&h.task.title)
    );
    let _ = writeln!(out, "  status    {}", styled_status(h.task.status));
    if let Some(branch) = branch {
        let _ = writeln!(out, "  branch    {branch}");
    }
    let _ = writeln!(
        out,
        "  tokens    {}",
        lower_bound(
            format!(
                "{} in / {} out",
                human_tokens(h.totals.usage.input_tokens),
                human_tokens(h.totals.usage.output_tokens)
            ),
            h.totals.complete
        )
    );
    let _ = writeln!(
        out,
        "  active    {}",
        lower_bound(
            human_duration(Duration::from_millis(h.totals.active_ms)),
            h.totals.complete
        )
    );
    if let Some(cost) = &h.cost {
        let _ = writeln!(out, "  cost      {}", cost_text(cost));
    }
    let _ = writeln!(out, "  activity  {}", relative_time(h.last_activity));

    let _ = writeln!(out, "\n{}", head(&format!("Runs ({})", h.runs.len())));
    if h.runs.is_empty() {
        let _ = writeln!(out, "  none");
    }
    for run in &h.runs {
        let status = run
            .status
            .map(|s| format!(" → {}", styled_status(s)))
            .unwrap_or_default();
        let resumes = match run.resumes {
            0 => String::new(),
            1 => ", resumed once".to_string(),
            n => format!(", resumed {n} times"),
        };
        let _ = writeln!(
            out,
            "  {} {}{status}, started {}{resumes}",
            style::bold().apply_to(run.run.short()),
            run_state(run),
            relative_time(run.started_at)
        );
        if run.totals_known {
            let cost = h
                .cost
                .as_ref()
                .and_then(|_| {
                    let of_run: Vec<_> = events
                        .iter()
                        .filter(|e| e.event.run_id() == Some(run.run))
                        .cloned()
                        .collect();
                    vibe_pipeline::history::cost_of(&of_run, config)
                })
                .map(|c| format!(", {}", cost_text(&c)))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "    {} active, {} in / {} out tokens{cost}",
                human_duration(Duration::from_millis(run.active_ms)),
                human_tokens(run.usage.input_tokens),
                human_tokens(run.usage.output_tokens)
            );
        } else {
            let _ = writeln!(
                out,
                "    {}",
                style::dim().apply_to("tokens and active time unknown (logged before 0.5)")
            );
        }
        if !run.phases.is_empty() {
            let phases: Vec<String> = run
                .phases
                .iter()
                .map(|p| {
                    let mark = match p.success {
                        Some(true) => style::ok().apply_to("✓").to_string(),
                        Some(false) => style::err().apply_to("✗").to_string(),
                        None => style::warn().apply_to("…").to_string(),
                    };
                    format!("{} {mark}", p.phase)
                })
                .collect();
            let _ = writeln!(out, "    phases  {}", phases.join("  "));
        }
        if let Some(merge) = &run.merged {
            let _ = writeln!(
                out,
                "    merged  {} into {} ({})",
                merge.branch,
                merge.base,
                short_sha(&merge.commit)
            );
        }
        if let Some(gate) = &run.pending_approval {
            let _ = writeln!(
                out,
                "    {}",
                style::warn().apply_to(format!("waiting for approval of the {gate}"))
            );
        }
        if let Some(error) = &run.last_error {
            let _ = writeln!(
                out,
                "    {}",
                style::err().apply_to(format!("error: {}", truncate(error, 200)))
            );
        }
    }

    let commits: Vec<_> = h.runs.iter().flat_map(|r| &r.commits).collect();
    if !commits.is_empty() {
        let _ = writeln!(out, "\n{}", head(&format!("Commits ({})", commits.len())));
        for c in commits {
            let subtask = c
                .subtask
                .map(|id| {
                    let label = subtasks.get(&id).cloned().unwrap_or_else(|| id.short());
                    style::dim()
                        .apply_to(format!(" · subtask {label}"))
                        .to_string()
                })
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {}  {} {}{subtask}",
                style::bold().apply_to(short_sha(&c.commit)),
                truncate(c.message.lines().next().unwrap_or_default(), 72),
                style::dim().apply_to(format!("({} file(s))", c.files.len()))
            );
        }
    }

    let files = &h.changed_files;
    let approximate = if files.approximate {
        style::warn().apply_to(", approximate").to_string()
    } else {
        String::new()
    };
    let source = style::dim().apply_to(format!("from {}", source_text(&files.source)));
    let _ = writeln!(
        out,
        "\n{} {source}{approximate}",
        head(&format!("Files ({})", files.files.len()))
    );
    for f in &files.files {
        let path = match &f.old_path {
            Some(old) => format!("{old} → {}", f.path),
            None => f.path.clone(),
        };
        let _ = writeln!(out, "  {}  {path}", status_letter(f.status));
    }

    if let Some(run) = h.runs.iter().rev().find(|r| !r.validations.is_empty()) {
        let _ = writeln!(out, "\n{}", head("Validations"));
        for v in &run.validations {
            let target = if v.integration { " (integration)" } else { "" };
            let line = if v.passed {
                style::ok().apply_to(format!("  ✓ {}{target}", v.command))
            } else {
                let code = v
                    .exit_code
                    .map(|c| format!(", exit {c}"))
                    .unwrap_or_default();
                style::err().apply_to(format!("  ✗ {}{target}{code}", v.command))
            };
            let _ = writeln!(out, "{line}");
        }
    }

    if let Some(qa) = &h.last_qa {
        let verdict = enum_name(&qa.verdict);
        let verdict = match qa.verdict {
            vibe_core::QaVerdict::Approved => style::ok().apply_to(verdict),
            _ => style::warn().apply_to(verdict),
        };
        let _ = writeln!(
            out,
            "\n{} round {}: {verdict}, {} issue(s)",
            head("QA"),
            qa.round,
            qa.issues
        );
        if !qa.summary.is_empty() {
            let _ = writeln!(out, "  {}", truncate(&qa.summary, 200));
        }
    }

    if !h.errors.is_empty() {
        let _ = writeln!(out, "\n{}", head("Problems"));
        for e in &h.errors {
            let _ = writeln!(out, "  {}", style::err().apply_to(e));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_bounds_and_costs() {
        console::set_colors_enabled(false);
        assert_eq!(lower_bound("12k".into(), true), "12k");
        assert_eq!(lower_bound("12k".into(), false), "12k+");
        let cost = Cost {
            amount: 0.4249,
            currency: "USD".into(),
            complete: false,
        };
        assert_eq!(cost_text(&cost), "0.42 USD+");
    }
}
