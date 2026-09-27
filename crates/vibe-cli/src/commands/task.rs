//! `vibe task add|list|show|discard`

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::json;
use vibe_core::{SubtaskWorkspaces, Task, TaskStatus, TaskStore, WorkspaceProvider};
use vibe_pipeline::{FileTaskStore, PipelineStore};

use super::{resolve_task, task_number};
use crate::app::{self, Overrides, is_git_repo, load_config, open_store, worktree_location};
use crate::cli::TaskCommand;
use crate::util::{Table, Ui, enum_name, relative_time, style, styled_status, truncate};

/// Run a `vibe task` subcommand.
pub async fn run(root: &Path, cmd: TaskCommand, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    match cmd {
        TaskCommand::Add {
            title,
            description,
            description_file,
            labels,
        } => add(&store, title, description, description_file, labels, ui).await,
        TaskCommand::List { status, all } => list(&store, status.map(Into::into), all, ui).await,
        TaskCommand::Import { issue, forge } => import(root, &store, &issue, &forge, ui).await,
        TaskCommand::Show { reference } => show(root, &store, &reference, ui).await,
        TaskCommand::Discard { reference, yes } => discard(root, &store, &reference, yes, ui).await,
    }
}

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("cannot read the description from stdin")?;
    Ok(text)
}

fn description_text(inline: Option<String>, file: Option<PathBuf>) -> Result<String> {
    let text = match (inline, file) {
        (Some(d), _) if d == "-" => read_stdin()?,
        (Some(d), _) => d,
        (None, Some(p)) if p.as_os_str() == "-" => read_stdin()?,
        (None, Some(p)) => {
            std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()))?
        }
        (None, None) => String::new(),
    };
    Ok(text.trim().to_string())
}

async fn add(
    store: &Arc<FileTaskStore>,
    title: String,
    description: Option<String>,
    description_file: Option<PathBuf>,
    labels: Vec<String>,
    ui: Ui,
) -> Result<u8> {
    let title = title.trim().to_string();
    if title.is_empty() {
        anyhow::bail!("the task title must not be empty");
    }
    let mut task = Task::new(title, description_text(description, description_file)?);
    task.labels = labels;
    store.save_task(&task).await?;
    let entry = store.entry(task.id).await?;
    let number = entry.as_ref().map(|e| e.number);
    let dir = store.task_dir(task.id).await?;
    if ui.json {
        ui.print_json(&json!({
            "number": number,
            "id": task.id,
            "dir": dir,
            "task": task,
        }));
    } else {
        println!(
            "{} created task {} {}",
            style::ok().apply_to("✓"),
            style::bold().apply_to(format!("#{}", number.unwrap_or_default())),
            task.title
        );
        println!("  id   {}", task.id);
        println!("  dir  {}", dir.display());
        println!(
            "\nRun it with: vibe run {}",
            number.map_or_else(|| task.id.short(), |n| n.to_string())
        );
    }
    Ok(0)
}

async fn list(
    store: &Arc<FileTaskStore>,
    status: Option<TaskStatus>,
    all: bool,
    ui: Ui,
) -> Result<u8> {
    let mut rows = Vec::new();
    for task in store.list_tasks().await? {
        let keep = match status {
            Some(s) => task.status == s,
            None => all || !matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled),
        };
        if keep {
            rows.push((task_number(store, &task).await, task));
        }
    }
    rows.sort_by_key(|(n, _)| n.unwrap_or(u32::MAX));
    if ui.json {
        let items: Vec<_> = rows
            .iter()
            .map(|(n, t)| json!({"number": n, "task": t}))
            .collect();
        ui.print_json(&json!(items));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("No tasks. Create one with: vibe task add \"<title>\"");
        return Ok(0);
    }
    let mut table = Table::new(["#", "status", "complexity", "title", "updated"]);
    for (n, t) in &rows {
        table.row([
            n.map(|n| n.to_string()).unwrap_or_default(),
            styled_status(t.status),
            t.complexity
                .map(|c| enum_name(&c))
                .unwrap_or_else(|| "-".into()),
            truncate(&t.title, 60),
            style::dim()
                .apply_to(relative_time(t.updated_at))
                .to_string(),
        ]);
    }
    print!("{}", table.render());
    Ok(0)
}

async fn show(root: &Path, store: &Arc<FileTaskStore>, reference: &str, ui: Ui) -> Result<u8> {
    let task = resolve_task(store, reference).await?;
    let number = task_number(store, &task).await;
    let dir = store.task_dir(task.id).await?;
    let spec = store.load_spec(task.id).await?;
    let plan = store.load_plan(task.id).await?;
    let qa = store.load_qa_reports(task.id).await?;
    let run = store.load_run_state(task.id).await?;
    let config = load_config(root)?;
    let worktree = if app::uses_worktrees(&config.pipeline.workspace) && is_git_repo(root).await {
        let loc = worktree_location(root, &task);
        let git = vibe_workspace::Git::new(root);
        let branch_exists = git.branch_exists(&loc.branch).await.unwrap_or(false);
        let dir_exists = loc.path.is_dir();
        (branch_exists || dir_exists).then_some((loc, branch_exists, dir_exists))
    } else {
        None
    };

    if ui.json {
        ui.print_json(&json!({
            "number": number,
            "task": task,
            "dir": dir,
            "spec": spec,
            "plan": plan,
            "qa_reports": qa,
            "run": run,
            "worktree": worktree.as_ref().map(|(l, b, d)| json!({
                "branch": l.branch, "path": l.path, "branch_exists": b, "dir_exists": d
            })),
        }));
        return Ok(0);
    }

    let h = |s: &str| style::accent().apply_to(s).to_string();
    println!(
        "{} {}",
        style::bold().apply_to(format!("#{}", number.unwrap_or_default())),
        style::bold().apply_to(&task.title)
    );
    println!("  status      {}", styled_status(task.status));
    println!(
        "  complexity  {}",
        task.complexity
            .map(|c| enum_name(&c))
            .unwrap_or_else(|| "not assessed".into())
    );
    if !task.labels.is_empty() {
        println!("  labels      {}", task.labels.join(", "));
    }
    println!("  id          {}", task.id);
    println!("  created     {}", relative_time(task.created_at));
    println!("  updated     {}", relative_time(task.updated_at));
    if !task.description.is_empty() {
        println!("\n{}\n{}", h("Description"), indent(&task.description));
    }

    if let Some(spec) = &spec {
        println!("\n{}\n{}", h("Specification"), indent(&spec.summary));
        for r in &spec.requirements {
            println!(
                "  - {} {} {}",
                style::bold().apply_to(&r.id),
                r.description,
                style::dim().apply_to(format!("({})", enum_name(&r.kind)))
            );
            for a in &r.acceptance {
                println!("      ✓ {a}");
            }
        }
    }

    if let Some(plan) = &plan {
        println!(
            "\n{} {}",
            h("Plan"),
            style::dim().apply_to(format!(
                "({}/{} done)",
                plan.subtasks()
                    .filter(|s| s.status == vibe_core::SubtaskStatus::Done)
                    .count(),
                plan.len()
            ))
        );
        if !plan.approach.is_empty() {
            println!("{}", indent(&plan.approach));
        }
        for phase in &plan.phases {
            println!("  {}", style::bold().apply_to(&phase.name));
            for s in &phase.subtasks {
                println!("    {} {}", subtask_marker(s.status), s.title);
            }
        }
    }

    if let Some(report) = qa.last() {
        let verdict = enum_name(&report.verdict);
        let v = match report.verdict {
            vibe_core::QaVerdict::Approved => style::ok().apply_to(verdict),
            _ => style::warn().apply_to(verdict),
        };
        println!("\n{} round {}: {v}", h("QA"), report.round);
        if !report.summary.is_empty() {
            println!("{}", indent(&report.summary));
        }
        for issue in &report.issues {
            let location = match (&issue.file, issue.line) {
                (Some(f), Some(l)) => format!(" {f}:{l}"),
                (Some(f), None) => format!(" {f}"),
                _ => String::new(),
            };
            println!(
                "  - [{}] {}{}",
                enum_name(&issue.severity),
                issue.title,
                style::dim().apply_to(location)
            );
        }
    }

    if let Some(state) = &run {
        println!(
            "\n{}\n  {} at phase {} ({}), started {}",
            h("Last run"),
            enum_name(&state.status),
            state.current_phase,
            state.run_id.short(),
            relative_time(state.started_at)
        );
        if let Some(e) = &state.last_error {
            println!("  note: {}", truncate(e, 200));
        }
    }

    if let Some((loc, branch_exists, dir_exists)) = &worktree {
        println!("\n{}", h("Workspace"));
        if *branch_exists {
            println!("  branch    {}", loc.branch);
        }
        if *dir_exists {
            println!("  worktree  {}", loc.path.display());
        }
    }
    println!("\n{}\n  {}", h("Files"), dir.display());
    Ok(0)
}

fn subtask_marker(status: vibe_core::SubtaskStatus) -> String {
    use vibe_core::SubtaskStatus as S;
    match status {
        S::Pending => style::dim().apply_to("[ ]").to_string(),
        S::InProgress => style::warn().apply_to("[~]").to_string(),
        S::Done => style::ok().apply_to("[x]").to_string(),
        S::Failed => style::err().apply_to("[!]").to_string(),
        S::Skipped => style::dim().apply_to("[-]").to_string(),
    }
}

fn indent(text: &str) -> String {
    text.trim()
        .lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn discard(
    root: &Path,
    store: &Arc<FileTaskStore>,
    reference: &str,
    yes: bool,
    ui: Ui,
) -> Result<u8> {
    let task = resolve_task(store, reference).await?;
    let number = task_number(store, &task).await;
    let label = crate::util::task_label(number, &task);
    if !yes
        && !crate::util::confirm(
            &format!("Discard {label}, its workspace and every artefact?"),
            "--yes",
        )?
    {
        println!("Aborted.");
        return Ok(1);
    }

    let config = load_config(root)?;
    let workspace_name = config.pipeline.workspace.clone();
    let mut discarded_workspace = false;
    match workspace_name.as_str() {
        name if app::uses_worktrees(name) => {
            if is_git_repo(root).await {
                let ws = app::worktree_workspace(root, &task);
                vibe_workspace::GitSubtaskWorkspaces::new()
                    .discard_all(&ws)
                    .await
                    .context("cannot discard the task's subtask worktrees")?;
                vibe_workspace::GitWorktreeProvider::new()
                    .with_base_branch(config.base_branch.clone())
                    .discard(&ws)
                    .await
                    .context("cannot discard the task's worktree")?;
                discarded_workspace = true;
            }
        }
        "in_place" => {}
        _ => {
            // A plugin-provided workspace: open (reopen) it through the
            // provider, then discard it.
            let ctx = app::build_context(root, &Overrides::default()).await?;
            let outcome = async {
                let ws = ctx.workspace.open(root, &task).await?;
                ctx.workspace.discard(&ws).await
            }
            .await;
            ctx.shutdown().await;
            outcome.context("cannot discard the task's workspace")?;
            discarded_workspace = true;
        }
    }

    // The traced tool outputs are filed under the task directory's name,
    // which the index forgets with the task: remove them first. A failure
    // only warns, so the task is never left half discarded.
    if let Some(entry) = store.entry(task.id).await? {
        let dir = root
            .join(vibe_core::config::VIBE_DIR)
            .join(vibe_core::config::TOOL_OUTPUT_DIR)
            .join(entry.dir);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!(
                "{} cannot remove the tool outputs in {}: {e}",
                style::warn().apply_to("warning:"),
                dir.display()
            ),
        }
    }
    store.delete_task(task.id).await?;
    if ui.json {
        ui.print_json(&json!({
            "discarded": task.id,
            "number": number,
            "workspace_discarded": discarded_workspace,
        }));
    } else {
        println!("{} discarded {label}", style::ok().apply_to("✓"));
    }
    Ok(0)
}

/// `vibe task import`: create a task from an issue, once per issue.
async fn import(
    root: &Path,
    store: &Arc<FileTaskStore>,
    issue: &str,
    forge: &str,
    ui: Ui,
) -> Result<u8> {
    use crate::forge::{Forge, ForgeKind, parse_issue_ref};
    let issue_ref = parse_issue_ref(issue, ForgeKind::parse(forge)?)?;
    let source = vibe_core::TaskSource::Issue {
        provider: issue_ref.forge.name().to_string(),
        reference: issue_ref.to_string(),
    };
    for existing in store.list_tasks().await? {
        if existing.source == source {
            let number = task_number(store, &existing).await;
            anyhow::bail!(
                "{issue_ref} is already imported as {}",
                crate::util::task_label(number, &existing)
            );
        }
    }
    let config = load_config(root)?;
    let found = Forge::from_config(issue_ref.forge, &config)
        .issue(&issue_ref)
        .await?;
    if found.title.is_empty() {
        anyhow::bail!("{issue_ref} has no title");
    }
    let mut description = found.body.clone();
    if !description.is_empty() {
        description.push_str("\n\n");
    }
    description.push_str(&format!("Imported from {issue_ref} ({})", found.url));
    let mut task = Task::new(found.title, description);
    task.labels = found.labels;
    task.source = source;
    store.save_task(&task).await?;
    let number = task_number(store, &task).await;
    if ui.json {
        ui.print_json(&json!({"task": task, "number": number, "issue": found.url}));
    } else {
        println!(
            "{} created {} from {issue_ref}",
            style::ok().apply_to("✓"),
            crate::util::task_label(number, &task)
        );
    }
    Ok(0)
}
