//! `vibe tui`: a terminal board of the project's tasks with a live view of
//! the selected one.
//!
//! The UI is built on the event stream only (ADR-007): logged events come
//! from each task's `events.jsonl`, read again every tick, so runs started by
//! other processes show up too; streamed text comes live from the runs this
//! UI started through its [`RunManager`]. [`app`] holds the state and the key
//! handling, [`view`] draws it, and this module runs the loop and performs
//! the actions.

pub mod app;
pub mod view;

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use ratatui::crossterm::event::{self as term, KeyEventKind};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use vibe_core::{Task, TaskId, TaskStatus, TaskStore};
use vibe_pipeline::{FileTaskStore, PipelineStore, RunManager, RunOptions, RunReport};

use crate::app::{AppContext, Overrides, build_context, uses_worktrees, worktree_workspace};
use crate::commands::approval::decide;
use crate::util::status_name;
use app::{Action, App, TaskRow, Tone};

/// How often tasks and the selected task are read again.
const TICK: Duration = Duration::from_millis(500);

/// How long the key reader waits for a key before checking whether to stop.
const KEY_POLL: Duration = Duration::from_millis(100);

/// Run the terminal UI until the user quits.
pub async fn run(root: &Path) -> Result<u8> {
    if !std::io::stdout().is_terminal() {
        bail!("vibe tui needs an interactive terminal");
    }
    let ctx = build_context(root, &Overrides::default()).await?;
    let manager = RunManager::new(ctx.pipeline());
    let outcome = event_loop(&ctx, &manager).await;
    ratatui::restore();
    ctx.shutdown().await;
    outcome
}

/// Reads keys on a blocking thread until `stop` is set.
fn spawn_key_reader(stop: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<term::KeyEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match term::poll(KEY_POLL) {
                Ok(true) => {
                    if let Ok(term::Event::Key(key)) = term::read()
                        && key.kind != KeyEventKind::Release
                        && tx.send(key).is_err()
                    {
                        return;
                    }
                }
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
    rx
}

async fn event_loop(ctx: &AppContext, manager: &RunManager) -> Result<u8> {
    let mut app = App {
        project: ctx
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ..App::default()
    };
    let store = &ctx.store;
    let mut live = manager.subscribe();
    let mut runs: JoinSet<(TaskId, vibe_core::Result<RunReport>)> = JoinSet::new();
    refresh(&mut app, store, manager).await;

    let mut terminal = ratatui::init();
    let stop = Arc::new(AtomicBool::new(false));
    let mut keys = spawn_key_reader(Arc::clone(&stop));
    let mut tick = tokio::time::interval(TICK);
    let result = loop {
        if let Err(e) = terminal.draw(|frame| view::draw(frame, &app)) {
            break Err(e.into());
        }
        tokio::select! {
            key = keys.recv() => {
                let Some(key) = key else { break Ok(0) };
                app.message = None;
                let action = app.handle_key(key);
                if action == Action::Quit {
                    break Ok(0);
                }
                perform(action, &mut app, ctx, manager, &mut runs).await;
                refresh(&mut app, store, manager).await;
            }
            event = live.recv() => {
                if let Ok(envelope) = event {
                    app.apply_live(&envelope);
                }
            }
            Some(done) = runs.join_next() => {
                if let Ok((task, outcome)) = done {
                    report_run_end(&mut app, task, outcome);
                }
                refresh(&mut app, store, manager).await;
            }
            _ = tick.tick() => refresh(&mut app, store, manager).await,
        }
    };
    stop.store(true, Ordering::Relaxed);
    // Runs started here stop with the UI; they stay resumable.
    for task in manager.active() {
        manager.cancel(task);
    }
    while runs.join_next().await.is_some() {}
    result
}

fn report_run_end(app: &mut App, task: TaskId, outcome: vibe_core::Result<RunReport>) {
    app.own_runs.retain(|t| *t != task);
    let label = app
        .tasks
        .iter()
        .find(|t| t.id == task)
        .map_or_else(|| task.short(), TaskRow::label);
    match outcome {
        Ok(report) => {
            let tone = if report.is_success() {
                Tone::Good
            } else {
                Tone::Warn
            };
            app.say(
                tone,
                format!("task {label}: {}", status_name(report.final_status)),
            );
        }
        Err(e) => app.say(Tone::Bad, format!("task {label}: {}", e.message)),
    }
}

async fn perform(
    action: Action,
    app: &mut App,
    ctx: &AppContext,
    manager: &RunManager,
    runs: &mut JoinSet<(TaskId, vibe_core::Result<RunReport>)>,
) {
    let store = &ctx.store;
    let outcome: Result<Option<String>> = async {
        match action {
            Action::None | Action::Quit | Action::Refresh => Ok(None),
            Action::NewTask(title) => {
                let task = Task::new(title, "");
                store.save_task(&task).await?;
                Ok(Some(format!("created “{}”", task.title)))
            }
            Action::Start(id) | Action::Resume(id) => {
                let resume = matches!(action, Action::Resume(_));
                let handle = if resume {
                    manager.resume(id, RunOptions::default())?
                } else {
                    manager.start(id, RunOptions::default())?
                };
                app.own_runs.push(id);
                runs.spawn(async move { (id, handle.wait().await) });
                Ok(Some(
                    if resume { "resuming" } else { "running" }.to_string(),
                ))
            }
            Action::Cancel(id) => {
                if !manager.cancel(id) {
                    store.request_cancel(id).await?;
                }
                Ok(Some("cancelling after the current step".to_string()))
            }
            Action::Approve(id) => {
                let record = decide(store, id, true, String::new()).await?;
                Ok(Some(format!("{} approved: press R to resume", record.gate)))
            }
            Action::Reject(id, reason) => {
                let record = decide(store, id, false, reason).await?;
                Ok(Some(format!("{} rejected: press R to resume", record.gate)))
            }
            Action::LoadChanges(id) => {
                app.detail.changes = Some(changes(ctx, id).await?);
                Ok(None)
            }
        }
    }
    .await;
    match outcome {
        Ok(Some(text)) => app.say(Tone::Good, text),
        Ok(None) => {}
        Err(e) => app.say(Tone::Bad, format!("{e:#}")),
    }
}

/// Changes of the task's workspace, as reviewers see them.
async fn changes(ctx: &AppContext, id: TaskId) -> Result<String> {
    let task = ctx.store.load_task(id).await?;
    if uses_worktrees(ctx.workspace.name()) {
        let ws = worktree_workspace(&ctx.root, &task);
        if !ws.root.is_dir() {
            return Ok("The task has no workspace yet.".to_string());
        }
        return Ok(ctx.workspace.changes(&ws).await?);
    }
    let git = vibe_workspace::Git::new(&ctx.root);
    Ok(git.status_short().await.unwrap_or_default())
}

/// Read the board and the selected task again.
async fn refresh(app: &mut App, store: &Arc<FileTaskStore>, manager: &RunManager) {
    if let Err(e) = try_refresh(app, store, manager).await {
        app.say(Tone::Bad, format!("cannot read the tasks: {e}"));
    }
}

async fn try_refresh(
    app: &mut App,
    store: &Arc<FileTaskStore>,
    manager: &RunManager,
) -> vibe_core::Result<()> {
    let mut rows = Vec::new();
    for task in store.list_tasks().await? {
        let number = store.entry(task.id).await?.map(|e| e.number);
        let running = manager.is_active(task.id)
            || (!matches!(
                task.status,
                TaskStatus::Done | TaskStatus::Cancelled | TaskStatus::Backlog
            ) && store.is_running(task.id).await.unwrap_or(false));
        rows.push(TaskRow::new(&task, number, running));
    }
    app.set_tasks(rows);
    let Some(id) = app.selected_task().map(|t| t.id) else {
        return Ok(());
    };
    app.select_detail(id);
    let state = store.load_run_state(id).await?;
    if state.as_ref().map(|s| s.run_id) != app.selected_run() {
        // A new run: show it from its start.
        app.detail.activity.clear();
        app.detail.consumed = 0;
        app.detail.streaming.clear();
        app.detail.budget = None;
    }
    app.detail.state = state;
    app.detail.plan = store.load_plan(id).await?;
    let log = store.load_events(id).await?;
    app.apply_log(&log);
    Ok(())
}
