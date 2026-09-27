//! `vibe pr`: push a ready task's branch and open a pull request.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;
use vibe_core::{QaVerdict, SubtaskStatus, Task, TaskSource, TaskStatus, TaskStore};
use vibe_pipeline::{FileTaskStore, PipelineStore};
use vibe_workspace::Git;

use super::{resolve_task, task_number};
use crate::app::{load_config, open_store, uses_worktrees, worktree_location};
use crate::cli::PrArgs;
use crate::forge::{Forge, ForgeKind, parse_issue_ref, repo_from_remote};
use crate::util::{Ui, style};

/// Push the branch of a ready task and open its pull request.
pub async fn run(root: &Path, args: PrArgs, ui: Ui) -> Result<u8> {
    let config = load_config(root)?;
    if !uses_worktrees(&config.pipeline.workspace) {
        bail!("pull requests need a task branch: use the git_worktree or container workspace");
    }
    let store = open_store(root)?;
    let task = resolve_task(&store, &args.reference).await?;
    if task.status != TaskStatus::Ready {
        bail!(
            "the task is {}, not ready: run it until QA approves first",
            crate::util::status_name(task.status)
        );
    }
    let git = Git::new(root);
    let branch = worktree_location(root, &task).branch;
    if !git.branch_exists(&branch).await? {
        bail!("the task branch {branch} does not exist");
    }
    let base = match &args.base {
        Some(b) => b.clone(),
        None => match git.config_get(&format!("branch.{branch}.vibebase")).await? {
            Some(b) => b,
            None => match config.base_branch.clone() {
                Some(b) => b,
                None => git.default_branch().await?,
            },
        },
    };
    let remote_url = git
        .output(&["remote", "get-url", &args.remote])
        .await?
        .stdout
        .trim()
        .to_string();
    let repo = match &args.repo {
        Some(r) => r.clone(),
        None => repo_from_remote(&remote_url).with_context(|| {
            format!(
                "cannot tell the repository from remote `{}`; pass --repo",
                args.remote
            )
        })?,
    };
    let kind = match &args.forge {
        Some(f) => ForgeKind::parse(f)?,
        None if remote_url.contains("gitlab") => ForgeKind::GitLab,
        None => match &task.source {
            TaskSource::Issue { provider, .. } => {
                ForgeKind::parse(provider).unwrap_or(ForgeKind::GitHub)
            }
            _ => ForgeKind::GitHub,
        },
    };

    if !args.no_push {
        let pushed = git
            .output(&["push", "--set-upstream", &args.remote, &branch])
            .await?;
        if !pushed.success {
            bail!(
                "git push {} {branch} failed: {}",
                args.remote,
                pushed.stderr.trim()
            );
        }
    }
    let body = describe(&store, &task, kind, &repo).await?;
    let url = Forge::from_config(kind, &config)
        .open_pull_request(&repo, &branch, &base, &task.title, &body, args.draft)
        .await?;
    store
        .append_progress(task.id, &format!("Pull request opened: {url}"))
        .await?;
    if ui.json {
        ui.print_json(&json!({"url": url, "branch": branch, "base": base, "repo": repo}));
    } else {
        let number = task_number(&store, &task).await;
        println!(
            "{} pull request for {}: {url}",
            style::ok().apply_to("✓"),
            crate::util::task_label(number, &task)
        );
    }
    Ok(0)
}

/// Description of the pull request: what the spec asked, the plan as done,
/// the last review and the required checks, and the issue it closes.
async fn describe(
    store: &FileTaskStore,
    task: &Task,
    kind: ForgeKind,
    repo: &str,
) -> Result<String> {
    let mut out = String::from("## Summary\n\n");
    match store.load_spec(task.id).await? {
        Some(spec) if !spec.summary.trim().is_empty() => {
            out.push_str(spec.summary.trim());
            out.push_str("\n\n");
            for r in &spec.requirements {
                out.push_str(&format!("- {}: {}\n", r.id, r.description.trim()));
            }
        }
        _ => {
            let description = task
                .description
                .lines()
                .take_while(|l| !l.starts_with("Imported from "))
                .collect::<Vec<_>>()
                .join("\n");
            out.push_str(if description.trim().is_empty() {
                &task.title
            } else {
                description.trim()
            });
            out.push('\n');
        }
    }
    if let Some(plan) = store.load_plan(task.id).await? {
        out.push_str("\n## Changes\n\n");
        for s in plan.subtasks() {
            let mark = if s.status == SubtaskStatus::Done {
                "x"
            } else {
                " "
            };
            out.push_str(&format!("- [{mark}] {}\n", s.title));
        }
    }
    if let Some(qa) = store.load_qa_reports(task.id).await?.last() {
        out.push_str(&format!(
            "\n## Review\n\nQA round {}: {}. {}\n",
            qa.round,
            match qa.verdict {
                QaVerdict::Approved => "approved",
                QaVerdict::ChangesRequested => "changes requested",
                QaVerdict::Inconclusive => "inconclusive",
            },
            qa.summary.trim()
        ));
    }
    if let Some(state) = store.load_run_state(task.id).await? {
        let checks: Vec<String> = state
            .validations
            .iter()
            .rev()
            .filter(|v| !v.integration)
            .map(|v| format!("- {} `{}`", if v.passed { "✓" } else { "✗" }, v.command))
            .collect();
        if !checks.is_empty() {
            out.push_str("\nRequired checks:\n");
            let mut seen = std::collections::HashSet::new();
            for line in checks.into_iter().rev() {
                if seen.insert(line.clone()) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
    }
    if let TaskSource::Issue {
        provider,
        reference,
    } = &task.source
        && let Ok(issue) = parse_issue_ref(reference, ForgeKind::parse(provider)?)
        && issue.forge == kind
    {
        out.push('\n');
        if issue.repo == repo {
            out.push_str(&format!("Closes #{}\n", issue.number));
        } else {
            out.push_str(&format!("Closes {issue}\n"));
        }
    }
    out.push_str("\n_Opened by Vibe Factory._\n");
    Ok(out)
}
