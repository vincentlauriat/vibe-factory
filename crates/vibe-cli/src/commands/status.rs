//! `vibe status`

use std::path::Path;

use anyhow::Result;
use serde_json::json;
use vibe_core::TaskStore;
use vibe_pipeline::PipelineStore;

use super::{is_initialised, task_number};
use crate::app::{is_git_repo, open_store};
use crate::util::{
    ALL_STATUSES, Table, Ui, enum_name, relative_time, status_name, style, styled_status, truncate,
};

/// Number of recent runs shown.
const RECENT_RUNS: usize = 5;

/// Print a project summary.
pub async fn run(root: &Path, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    let tasks = store.list_tasks().await?;

    let counts: Vec<(vibe_core::TaskStatus, usize)> = ALL_STATUSES
        .iter()
        .map(|s| (*s, tasks.iter().filter(|t| t.status == *s).count()))
        .collect();

    let mut runs = Vec::new();
    for task in &tasks {
        if let Some(state) = store.load_run_state(task.id).await? {
            runs.push((task_number(&store, task).await, task.clone(), state));
        }
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.2.updated_at));
    runs.truncate(RECENT_RUNS);

    // Task worktrees are recognised by their branch prefix: comparing paths
    // is unreliable (symlinked temp dirs, `\\?\` and 8.3 names on Windows).
    let worktrees: Vec<vibe_workspace::WorktreeInfo> = if is_git_repo(root).await {
        vibe_workspace::Git::new(root)
            .worktree_list()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.branch.as_deref().is_some_and(|b| b.starts_with("vibe/")))
            .collect()
    } else {
        Vec::new()
    };

    if ui.json {
        let by_status: serde_json::Map<String, serde_json::Value> = counts
            .iter()
            .map(|(s, n)| (status_name(*s).to_string(), json!(n)))
            .collect();
        ui.print_json(&json!({
            "root": root,
            "initialised": is_initialised(root),
            "tasks": tasks.len(),
            "by_status": by_status,
            "recent_runs": runs.iter().map(|(n, t, s)| json!({
                "number": n, "task": t.title, "task_id": t.id, "run": s
            })).collect::<Vec<_>>(),
            "worktrees": worktrees.iter().map(|w| json!({
                "path": w.path, "branch": w.branch
            })).collect::<Vec<_>>(),
        }));
        return Ok(0);
    }

    println!("{} {}", style::bold().apply_to("Project"), root.display());
    if !is_initialised(root) {
        println!(
            "  {} not initialised (run `vibe init`)",
            style::warn().apply_to("!")
        );
    }
    let summary: Vec<String> = counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(s, n)| format!("{n} {}", styled_status(*s)))
        .collect();
    println!(
        "\n{} {}",
        style::accent().apply_to("Tasks"),
        if summary.is_empty() {
            "none".to_string()
        } else {
            format!("{} — {}", tasks.len(), summary.join(", "))
        }
    );

    if !runs.is_empty() {
        println!("\n{}", style::accent().apply_to("Recent runs"));
        let mut table = Table::new(["#", "task", "run", "phase", "updated"]);
        for (n, t, s) in &runs {
            table.row([
                n.map(|n| n.to_string()).unwrap_or_default(),
                truncate(&t.title, 50),
                enum_name(&s.status),
                s.current_phase.to_string(),
                relative_time(s.updated_at),
            ]);
        }
        print!("{}", table.render());
    }

    if !worktrees.is_empty() {
        println!("\n{}", style::accent().apply_to("Active worktrees"));
        for w in &worktrees {
            println!(
                "  {}  {}",
                w.branch.as_deref().unwrap_or("(detached)"),
                style::dim().apply_to(w.path.display())
            );
        }
    }
    Ok(0)
}
