//! `vibe tui`: a terminal board of the project's tasks with a live view of
//! the selected one.
//!
//! The UI is built on the event stream only (ADR-007): logged events come
//! from each task's `events.jsonl`, read incrementally every tick, so runs
//! started by other processes show up too; streamed text comes live from the
//! runs this UI started through its [`RunManager`]. [`app`] and [`panes`]
//! hold the state and the key handling, [`view`] draws it, and this module
//! runs the loop, performs the actions and loads the Activity, History and
//! Trace data in background tasks.

pub mod app;
pub mod panes;
pub mod view;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use ratatui::crossterm::event::{self as term, KeyEventKind};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use vibe_core::{Envelope, RunId, Task, TaskId, TaskStatus, TaskStore, VibeConfig};
use vibe_pipeline::history::{HistoryFilter, TaskHistory, project_history};
use vibe_pipeline::store::EVENTS_FILE;
use vibe_pipeline::{
    AllEventsFollower, EventReader, FileTaskStore, PipelineStore, PollResult, RunManager,
    RunOptions, RunReport, TaggedEnvelope, read_output,
};

use crate::app::{AppContext, Overrides, build_context, uses_worktrees, worktree_workspace};
use crate::commands::approval::decide;
use crate::commands::history::task_branch;
use crate::util::status_name;
use app::{Action, App, Load, Screen, TaskRow, Tone};
use panes::{FEED_ON_ENTRY, TraceData};

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

/// Incremental reader of the selected task's log. It starts again from the
/// log's start when another task is selected or its last run changes, since
/// the activity shows one run.
#[derive(Debug, Default)]
pub(crate) struct SelectedLog {
    task: Option<TaskId>,
    run: Option<RunId>,
    reader: Option<EventReader>,
}

impl SelectedLog {
    /// Events of `task` appended since the last call, and whether the log
    /// was read again from its start (what was shown must be forgotten).
    pub(crate) async fn read(
        &mut self,
        store: &FileTaskStore,
        task: TaskId,
        run: Option<RunId>,
    ) -> vibe_core::Result<(bool, Vec<Envelope>)> {
        let restart = self.reader.is_none() || self.task != Some(task) || self.run != run;
        if restart {
            // The new reader is kept only once it has read: after a failure
            // the next call starts again, and still reports the restart.
            let path = store.task_dir(task).await?.join(EVENTS_FILE);
            let mut reader = EventReader::new(path);
            let events = reader.read_new().await?;
            self.task = Some(task);
            self.run = run;
            self.reader = Some(reader);
            return Ok((true, events));
        }
        let reader = self.reader.as_mut().expect("reader is set");
        let before = reader.offset();
        let events = reader.read_new().await?;
        // The reader went back: the log was truncated or replaced and is
        // read from its start, so what was shown must be forgotten. (A
        // replacement longer than what was read is not seen here.)
        Ok((reader.offset() < before, events))
    }
}

/// Answers of the background loads.
enum Loaded {
    Activity {
        id: u64,
        result: vibe_core::Result<(AllEventsFollower, PollResult)>,
    },
    History {
        id: u64,
        result: Result<Vec<TaskHistory>, String>,
    },
    Trace {
        id: u64,
        result: Result<TraceData, String>,
    },
    Output {
        id: u64,
        result: Result<Option<String>, String>,
    },
}

/// What the background loads need from the context.
#[derive(Clone)]
struct Loader {
    store: Arc<FileTaskStore>,
    root: PathBuf,
    config: VibeConfig,
    tx: mpsc::UnboundedSender<Loaded>,
}

impl Loader {
    fn start(&self, load: Load) {
        let this = self.clone();
        tokio::spawn(async move {
            let loaded = match load {
                Load::Activity(id) => Loaded::Activity {
                    id,
                    result: last_events(&this.store, FEED_ON_ENTRY).await,
                },
                Load::History { id, all } => Loaded::History {
                    id,
                    result: this.history(all).await.map_err(|e| e.to_string()),
                },
                Load::Trace { id, task } => Loaded::Trace {
                    id,
                    result: this
                        .store
                        .load_events(task)
                        .await
                        .map(|events| TraceData::from_events(&events, &this.root))
                        .map_err(|e| e.to_string()),
                },
                Load::Output { id, call } => Loaded::Output {
                    id,
                    result: read_output(&this.root, &call)
                        .await
                        .map_err(|e| e.to_string()),
                },
            };
            let _ = this.tx.send(loaded);
        });
    }

    async fn history(&self, all: bool) -> vibe_core::Result<Vec<TaskHistory>> {
        let (root, config) = (&self.root, &self.config);
        let branch_of = move |task: &Task| task_branch(root, config, task);
        project_history(
            self.store.as_ref(),
            &self.root,
            &self.config,
            HistoryFilter { all },
            &branch_of,
        )
        .await
    }
}

/// The last `n` events of every task, and a follower of the ones logged
/// after them. Logs are read one at a time and only their last `n` events
/// are kept, so memory stays bounded by the largest log, not the whole
/// history; the follower starts after the newest event kept, and its first
/// poll (done here too) brings what was logged meanwhile.
pub(crate) async fn last_events(
    store: &FileTaskStore,
    n: usize,
) -> vibe_core::Result<(AllEventsFollower, PollResult)> {
    let mut kept: Vec<TaggedEnvelope> = Vec::new();
    let mut errors = Vec::new();
    for (task, entry) in store.entries().await? {
        let path = store.root().join(&entry.dir).join(EVENTS_FILE);
        match EventReader::new(path).read_new().await {
            Ok(events) => {
                let skip = events.len().saturating_sub(n);
                kept.extend(
                    events
                        .into_iter()
                        .skip(skip)
                        .map(|envelope| TaggedEnvelope {
                            task,
                            number: entry.number,
                            envelope,
                        }),
                );
            }
            Err(e) => errors.push((task, e)),
        }
    }
    kept.sort_by_key(TaggedEnvelope::cursor);
    kept.drain(..kept.len().saturating_sub(n));
    let mut follower = AllEventsFollower::new(store, kept.last().map(TaggedEnvelope::cursor));
    let mut first = follower.poll().await?;
    kept.append(&mut first.events);
    errors.append(&mut first.errors);
    Ok((
        follower,
        PollResult {
            events: kept,
            errors,
        },
    ))
}

/// Hand a background answer to the app; the Activity follower is kept when
/// its answer is taken.
fn take_loaded(app: &mut App, loaded: Loaded, follower: &mut Option<AllEventsFollower>) {
    match loaded {
        Loaded::Activity { id, result } => match result {
            Ok((f, first)) => {
                if app.set_feed(id, Ok(first.events)) {
                    *follower = Some(f);
                    report_poll_errors(app, &first.errors);
                }
            }
            Err(e) => {
                app.set_feed(id, Err(e.to_string()));
            }
        },
        Loaded::History { id, result } => app.set_history(id, result),
        Loaded::Trace { id, result } => app.set_trace(id, result),
        Loaded::Output { id, result } => app.set_output(id, result),
    }
}

fn report_poll_errors(app: &mut App, errors: &[(TaskId, vibe_core::Error)]) {
    for (task, error) in errors {
        let number = app
            .tasks
            .iter()
            .find(|t| t.id == *task)
            .and_then(|t| t.number);
        app.report_feed_error(*task, number, &error.to_string());
    }
}

/// New events for the Activity screen, while it is shown.
async fn poll_feed(app: &mut App, follower: &mut Option<AllEventsFollower>) {
    if app.screen != Screen::Activity {
        return;
    }
    let Some(f) = follower.as_mut() else {
        return;
    };
    match f.poll().await {
        Ok(result) => {
            app.report_index_error(None);
            app.push_feed(&result.events);
            report_poll_errors(app, &result.errors);
        }
        Err(e) => app.report_index_error(Some(e.to_string())),
    }
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
    let mut log = SelectedLog::default();
    let (tx, mut loaded) = mpsc::unbounded_channel();
    let loader = Loader {
        store: Arc::clone(store),
        root: ctx.root.clone(),
        config: ctx.config.clone(),
        tx,
    };
    let mut follower: Option<AllEventsFollower> = None;
    refresh(&mut app, store, manager, &mut log).await;

    let mut terminal = ratatui::init();
    let stop = Arc::new(AtomicBool::new(false));
    let mut keys = spawn_key_reader(Arc::clone(&stop));
    let mut tick = tokio::time::interval(TICK);
    let result = loop {
        for load in app.take_loads() {
            if matches!(load, Load::Activity(_)) {
                // The new load brings its own follower; the old one must not
                // catch up with the backlog in the loop meanwhile.
                follower = None;
            }
            loader.start(load);
        }
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
                refresh(&mut app, store, manager, &mut log).await;
            }
            event = live.recv() => {
                if let Ok(envelope) = event {
                    app.apply_live(&envelope);
                }
            }
            Some(answer) = loaded.recv() => take_loaded(&mut app, answer, &mut follower),
            Some(done) = runs.join_next() => {
                if let Ok((task, outcome)) = done {
                    report_run_end(&mut app, task, outcome);
                }
                refresh(&mut app, store, manager, &mut log).await;
            }
            _ = tick.tick() => {
                refresh(&mut app, store, manager, &mut log).await;
                poll_feed(&mut app, &mut follower).await;
            }
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
pub(crate) async fn changes(ctx: &AppContext, id: TaskId) -> Result<String> {
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
async fn refresh(
    app: &mut App,
    store: &Arc<FileTaskStore>,
    manager: &RunManager,
    log: &mut SelectedLog,
) {
    if let Err(e) = try_refresh(app, store, manager, log).await {
        app.say(Tone::Bad, format!("cannot read the tasks: {e}"));
    }
}

async fn try_refresh(
    app: &mut App,
    store: &Arc<FileTaskStore>,
    manager: &RunManager,
    log: &mut SelectedLog,
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
    app.detail.state = store.load_run_state(id).await?;
    app.detail.plan = store.load_plan(id).await?;
    let (restart, fresh) = log.read(store, id, app.selected_run()).await?;
    if restart {
        // Another task or a new run: show it from its start.
        app.restart_log();
    }
    app.apply_log(&fresh);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::{Event, Phase};

    fn phase(run: RunId, phase: Phase) -> String {
        let line = serde_json::to_string(&Envelope::now(Event::PhaseStarted { run, phase }));
        format!("{}\n", line.unwrap())
    }

    #[tokio::test]
    async fn the_selected_log_is_read_incrementally_and_again_on_a_new_selection() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let (a, b) = (Task::new("A", ""), Task::new("B", ""));
        store.save_task(&a).await.unwrap();
        store.save_task(&b).await.unwrap();
        let path = store.task_dir(a.id).await.unwrap().join(EVENTS_FILE);
        let run = RunId::new();
        std::fs::write(&path, phase(run, Phase::Spec)).unwrap();

        let mut log = SelectedLog::default();
        let (restart, events) = log.read(&store, a.id, Some(run)).await.unwrap();
        assert!(restart);
        assert_eq!(events.len(), 1);
        let (restart, events) = log.read(&store, a.id, Some(run)).await.unwrap();
        assert!(!restart);
        assert!(events.is_empty(), "nothing new");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(phase(run, Phase::Plan).as_bytes())
            .unwrap();
        let (restart, events) = log.read(&store, a.id, Some(run)).await.unwrap();
        assert!(!restart);
        assert_eq!(events.len(), 1, "only the appended event");

        // Another task, then back: each starts from its log's start.
        let (restart, events) = log.read(&store, b.id, None).await.unwrap();
        assert!(restart);
        assert!(events.is_empty());
        let (restart, events) = log.read(&store, a.id, Some(run)).await.unwrap();
        assert!(restart);
        assert_eq!(events.len(), 2);
        // A new run of the same task too.
        let (restart, events) = log.read(&store, a.id, Some(RunId::new())).await.unwrap();
        assert!(restart);
        assert_eq!(events.len(), 2);

        // A log that cannot be read right after a restart keeps the restart
        // pending: the next successful read still reports it.
        let other = RunId::new();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(log.read(&store, a.id, Some(other)).await.is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, phase(other, Phase::Spec)).unwrap();
        let (restart, fresh) = log.read(&store, a.id, Some(other)).await.unwrap();
        assert!(restart, "the failed restart is not forgotten");
        assert_eq!(fresh.len(), 1);

        // A log truncated under the reader is read again from its start.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(phase(other, Phase::Plan).as_bytes())
            .unwrap();
        let (restart, fresh) = log.read(&store, a.id, Some(other)).await.unwrap();
        assert!(!restart);
        assert_eq!(fresh.len(), 1);
        std::fs::write(&path, phase(other, Phase::Qa)).unwrap();
        let (restart, fresh) = log.read(&store, a.id, Some(other)).await.unwrap();
        assert!(restart, "the reader went back");
        assert_eq!(fresh.len(), 1);

        // Through the app: a restart forgets the activity shown.
        let mut app = App::default();
        app.set_tasks(vec![TaskRow::new(&a, Some(1), false)]);
        app.select_detail(a.id);
        app.detail.state = Some(vibe_pipeline::RunState::new(run, a.id, Phase::Plan));
        app.apply_log(&events);
        assert_eq!(app.detail.activity.len(), 2);
        app.restart_log();
        assert!(app.detail.activity.is_empty());
    }

    #[tokio::test]
    async fn activity_opens_on_the_last_events_and_follows_the_next_ones() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let (a, b) = (Task::new("A", ""), Task::new("B", ""));
        store.save_task(&a).await.unwrap();
        store.save_task(&b).await.unwrap();
        let run = RunId::new();
        let mut paths = Vec::new();
        for (task, count) in [(a.id, 5), (b.id, 3)] {
            let path = store.task_dir(task).await.unwrap().join(EVENTS_FILE);
            let log: String = (0..count).map(|_| phase(run, Phase::Build)).collect();
            std::fs::write(&path, log).unwrap();
            paths.push(path);
        }
        let (mut follower, first) = last_events(&store, 4).await.unwrap();
        assert_eq!(first.events.len(), 4, "the newest 4 of 8");
        assert!(first.errors.is_empty());
        let cursors: Vec<_> = first.events.iter().map(TaggedEnvelope::cursor).collect();
        assert!(cursors.windows(2).all(|w| w[0] <= w[1]), "in order");

        assert!(
            follower.poll().await.unwrap().events.is_empty(),
            "nothing twice"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&paths[1])
            .unwrap()
            .write_all(phase(run, Phase::Qa).as_bytes())
            .unwrap();
        let next = follower.poll().await.unwrap().events;
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].task, b.id);
    }
}
