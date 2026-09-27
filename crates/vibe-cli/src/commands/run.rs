//! `vibe run`

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::watch;
use vibe_core::{TaskStatus, TaskStore};
use vibe_pipeline::{RunOptions, RunReport, RunStatus};

use super::{resolve_task, task_number};
use crate::app::{Overrides, build_context, worktree_location};
use crate::cli::RunArgs;
use crate::render::{Renderer, SummaryInfo, summary_json, summary_text};
use crate::util::{Ui, style};

/// Exit code when the run was cancelled (conventional for `Ctrl-C`).
pub const EXIT_CANCELLED: u8 = 130;

/// Exit code of a run:
///
/// | Outcome | Code |
/// |---------|------|
/// | ready / done | 0 |
/// | stopped as requested (`--dry-run`, `--until`) | 0 |
/// | waiting in review, or paused for a human | 2 |
/// | cancelled | 130 |
/// | failed | 1 |
pub fn exit_code(report: &RunReport, stop_requested: bool) -> u8 {
    match report.final_status {
        TaskStatus::Ready | TaskStatus::Done => 0,
        TaskStatus::Cancelled => EXIT_CANCELLED,
        TaskStatus::Failed => 1,
        _ if report.state.status == RunStatus::Paused
            && stop_requested
            && report.final_status == TaskStatus::Backlog =>
        {
            0
        }
        TaskStatus::Review => 2,
        _ if report.state.status == RunStatus::Paused => 2,
        _ => 1,
    }
}

/// Run (or resume) a task through the pipeline.
pub async fn run(root: &Path, args: RunArgs, ui: Ui) -> Result<u8> {
    let overrides = Overrides {
        provider: args.provider.clone(),
        model: args.model.clone(),
        workspace: args.workspace.clone(),
        auto_merge: args.auto_merge,
        script: args.script.clone(),
    };
    let ctx = build_context(root, &overrides).await?;
    let outcome = execute(&ctx, &args, ui).await;
    ctx.shutdown().await;
    outcome
}

async fn execute(ctx: &crate::app::AppContext, args: &RunArgs, ui: Ui) -> Result<u8> {
    let task = resolve_task(&ctx.store, &args.reference).await?;
    let number = task_number(&ctx.store, &task).await;
    let store: Arc<dyn TaskStore> = ctx.store.clone();
    let renderer = Arc::new(Renderer::new(ui.json, ui.verbose, task.id, store));
    ctx.events.add_sink(renderer.clone()).await;

    if !ui.json {
        let (model, _) = ctx.config.model_for(vibe_core::Phase::Build);
        println!(
            "{} {} {}",
            style::bold().apply_to(if args.resume { "Resuming" } else { "Running" }),
            style::bold().apply_to(crate::util::task_label(number, &task)),
            style::dim().apply_to(format!(
                "(model {model}, workspace {}{})",
                ctx.workspace.name(),
                if args.dry_run { ", dry run" } else { "" }
            ))
        );
    }

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let ctrl_c = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        eprintln!(
            "\n{} cancelling after the current step… press Ctrl-C again to exit now",
            style::warn().apply_to("!")
        );
        let _ = cancel_tx.send(true);
        if tokio::signal::ctrl_c().await.is_ok() {
            std::process::exit(i32::from(EXIT_CANCELLED));
        }
    });

    let options = RunOptions {
        complexity_override: args.complexity.map(Into::into),
        from_phase: args.from.map(Into::into),
        until_phase: args.until.map(Into::into),
        dry_run: args.dry_run,
        cancel: Some(cancel_rx),
    };
    let pipeline = ctx.pipeline();
    let result = if args.resume {
        pipeline.resume_with(task.id, options).await
    } else {
        pipeline.run(task.id, options).await
    };
    ctrl_c.abort();
    let report = result?;

    let code = exit_code(&report, args.dry_run || args.until.is_some());
    let mut info = SummaryInfo {
        task_dir: ctx.store.task_dir(task.id).await.ok(),
        ..SummaryInfo::default()
    };
    if ctx.workspace.name() == "git_worktree" {
        let loc = worktree_location(&ctx.root, &report.task);
        if loc.path.is_dir() {
            info.branch = Some(loc.branch);
            info.worktree = Some(loc.path);
        }
    }
    if ui.json {
        ui.print_json(&summary_json(&report, &info, code));
    } else {
        print!(
            "{}",
            summary_text(&report, &renderer.phase_durations(), &info)
        );
        if let Some(hint) = next_step_hint(&report, code, number, &info) {
            println!("\n{hint}");
        }
    }
    Ok(code)
}

fn next_step_hint(
    report: &RunReport,
    code: u8,
    number: Option<u32>,
    info: &SummaryInfo,
) -> Option<String> {
    let reference = number.map_or_else(|| report.task.id.short(), |n| n.to_string());
    match (report.final_status, code) {
        (TaskStatus::Ready, _) => Some(match &info.branch {
            Some(b) => format!("Review the work, then merge it: git merge {b}"),
            None => format!("Review the work: vibe task show {reference}"),
        }),
        (TaskStatus::Done, _) => None,
        (_, 0) | (_, 2) | (TaskStatus::Cancelled, _) => {
            Some(format!("Continue with: vibe run {reference} --resume"))
        }
        _ => Some(format!(
            "Inspect with: vibe task show {reference}; retry with: vibe run {reference} --resume"
        )),
    }
}
