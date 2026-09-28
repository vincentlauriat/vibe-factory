//! State of the terminal UI and its reactions to keys and events.
//!
//! Everything here is synchronous and free of I/O, so that it can be tested
//! without a terminal: keys become [`Action`]s that the event loop performs,
//! and events update the view of the selected task.

use std::collections::VecDeque;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use vibe_core::{
    ApprovalGate, Artefact, Envelope, Event, Plan, RunId, StreamDelta, SubtaskId, Task, TaskId,
    TaskStatus,
};
use vibe_pipeline::history::TaskHistory;
use vibe_pipeline::trace::Call;
use vibe_pipeline::{RunState, TaggedEnvelope};

use super::panes::{
    FEED_ON_ENTRY, Feed, FeedLine, Group, HistoryView, LoadState, TraceData, TraceView,
};
use crate::util::{enum_name, human_duration, human_tokens, short_sha, status_name, truncate};

/// Activity lines kept for the selected task.
pub const ACTIVITY_LIMIT: usize = 500;

/// Streamed text kept while a step is being generated.
pub const STREAM_LIMIT: usize = 4000;

/// One row of the task board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    /// Task id.
    pub id: TaskId,
    /// Number in the store.
    pub number: Option<u32>,
    /// Title.
    pub title: String,
    /// Status.
    pub status: TaskStatus,
    /// Whether a process runs it now.
    pub running: bool,
}

impl TaskRow {
    /// Row of `task`.
    #[must_use]
    pub fn new(task: &Task, number: Option<u32>, running: bool) -> Self {
        Self {
            id: task.id,
            number,
            title: task.title.clone(),
            status: task.status,
            running,
        }
    }

    /// `3`, or a short id when the task has no number.
    #[must_use]
    pub fn label(&self) -> String {
        self.number
            .map_or_else(|| self.id.short(), |n| format!("{n:03}"))
    }
}

/// How an activity line is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Normal text.
    Normal,
    /// Secondary detail.
    Dim,
    /// Success.
    Good,
    /// Needs attention.
    Warn,
    /// Failure.
    Bad,
}

/// One line of the activity view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// Sequence number of the event it comes from.
    pub seq: Option<u64>,
    /// How to show it.
    pub tone: Tone,
    /// Text.
    pub text: String,
}

/// Budget consumption of the selected run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Budget {
    /// Tokens used.
    pub tokens: u64,
    /// Token limit.
    pub token_limit: Option<u64>,
    /// Active time in milliseconds.
    pub active_ms: u64,
    /// Duration limit in milliseconds.
    pub duration_limit_ms: Option<u64>,
}

/// Top-level screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// Task board and the selected task (`T`).
    #[default]
    Tasks,
    /// Events of every task (`A`).
    Activity,
    /// Finished tasks (`H`).
    History,
}

impl Screen {
    /// Every screen, in display order.
    pub const ALL: [Screen; 3] = [Screen::Tasks, Screen::Activity, Screen::History];

    /// Title, with the key that opens it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Screen::Tasks => "T Tasks",
            Screen::Activity => "A Activity",
            Screen::History => "H History",
        }
    }
}

/// Tabs of the detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// Live events.
    #[default]
    Activity,
    /// Subtasks of the plan.
    Plan,
    /// Changes in the workspace.
    Changes,
    /// Tool calls of a run.
    Trace,
}

impl Tab {
    /// Every tab, in display order.
    pub const ALL: [Tab; 4] = [Tab::Activity, Tab::Plan, Tab::Changes, Tab::Trace];

    /// Title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Tab::Activity => "Activity",
            Tab::Plan => "Plan",
            Tab::Changes => "Changes",
            Tab::Trace => "Trace",
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::Activity => Tab::Plan,
            Tab::Plan => Tab::Changes,
            Tab::Changes => Tab::Trace,
            Tab::Trace => Tab::Activity,
        }
    }
}

/// What the text input is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// Title of a new task.
    NewTask,
    /// Reason of a rejection.
    Reject(TaskId),
}

/// A line being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// Purpose.
    pub kind: InputKind,
    /// Text so far.
    pub buffer: String,
}

impl Input {
    /// Prompt shown before the text.
    #[must_use]
    pub fn prompt(&self) -> &'static str {
        match self.kind {
            InputKind::NewTask => "New task title",
            InputKind::Reject(_) => "Reason of the rejection",
        }
    }
}

/// What the event loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing.
    None,
    /// Leave the UI.
    Quit,
    /// Reload tasks and the selected task.
    Refresh,
    /// Create a task with this title.
    NewTask(String),
    /// Start a new run.
    Start(TaskId),
    /// Resume the last run.
    Resume(TaskId),
    /// Cancel the run.
    Cancel(TaskId),
    /// Approve what the run waits for.
    Approve(TaskId),
    /// Reject what the run waits for.
    Reject(TaskId, String),
    /// Load the changes of the task's workspace.
    LoadChanges(TaskId),
}

/// Data to load in the background, with the id its answer must carry.
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// The last events of every task, and a follower for the next ones.
    Activity(u64),
    /// The History rows.
    History {
        /// Request id.
        id: u64,
        /// Include failed and cancelled tasks.
        all: bool,
    },
    /// The tool calls of a task.
    Trace {
        /// Request id.
        id: u64,
        /// Task.
        task: TaskId,
    },
    /// The complete output of a call.
    Output {
        /// Request id.
        id: u64,
        /// The call.
        call: Box<Call>,
    },
}

/// Everything shown about the selected task.
#[derive(Debug, Clone, Default)]
pub struct Detail {
    /// Task the detail belongs to.
    pub task: Option<TaskId>,
    /// Last run state.
    pub state: Option<RunState>,
    /// Plan, if any.
    pub plan: Option<Plan>,
    /// Activity lines, oldest first.
    pub activity: VecDeque<Activity>,
    /// Text of the step being streamed.
    pub streaming: String,
    /// Budget, from the last `budget_updated`.
    pub budget: Option<Budget>,
    /// Changes of the workspace, once loaded.
    pub changes: Option<String>,
    /// Tool calls, once the Trace tab is opened.
    pub trace: TraceView,
}

/// The whole UI state.
#[derive(Debug, Clone, Default)]
pub struct App {
    /// Project name, for the title.
    pub project: String,
    /// Screen shown.
    pub screen: Screen,
    /// The Activity screen.
    pub feed: Feed,
    /// The History screen.
    pub history: HistoryView,
    /// Last request id handed out for a background load.
    pub requests: u64,
    /// Task board, newest first.
    pub tasks: Vec<TaskRow>,
    /// Index of the selected row.
    pub selected: usize,
    /// Selected tab.
    pub tab: Tab,
    /// Selected task.
    pub detail: Detail,
    /// Line being typed, if any.
    pub input: Option<Input>,
    /// Last message for the status line.
    pub message: Option<(Tone, String)>,
    /// Tasks run by this UI.
    pub own_runs: Vec<TaskId>,
}

impl App {
    /// Id of the selected task.
    #[must_use]
    pub fn selected_task(&self) -> Option<&TaskRow> {
        self.tasks.get(self.selected)
    }

    /// Replace the task board, keeping the same task selected when it still
    /// exists.
    pub fn set_tasks(&mut self, tasks: Vec<TaskRow>) {
        let current = self.selected_task().map(|t| t.id);
        self.tasks = tasks;
        self.selected = current
            .and_then(|id| self.tasks.iter().position(|t| t.id == id))
            .unwrap_or(0)
            .min(self.tasks.len().saturating_sub(1));
    }

    /// Show a message in the status line.
    pub fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.message = Some((tone, text.into()));
    }

    /// Run id of the selected task's last run.
    #[must_use]
    pub fn selected_run(&self) -> Option<RunId> {
        self.detail.state.as_ref().map(|s| s.run_id)
    }

    /// Gate the selected task waits for, if any.
    #[must_use]
    pub fn pending_approval(&self) -> Option<ApprovalGate> {
        self.detail.state.as_ref().and_then(|s| s.pending_approval)
    }

    /// React to a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if let Some(input) = self.input.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    Action::None
                }
                KeyCode::Enter => {
                    let input = self.input.take().expect("input is active");
                    let text = input.buffer.trim().to_string();
                    if text.is_empty() {
                        self.say(Tone::Warn, format!("{} cannot be empty", input.prompt()));
                        return Action::None;
                    }
                    match input.kind {
                        InputKind::NewTask => Action::NewTask(text),
                        InputKind::Reject(id) => Action::Reject(id, text),
                    }
                }
                KeyCode::Backspace => {
                    input.buffer.pop();
                    Action::None
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.buffer.push(c);
                    Action::None
                }
                _ => Action::None,
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('T') => {
                self.screen = Screen::Tasks;
                return Action::Refresh;
            }
            KeyCode::Char('A') => {
                if self.screen != Screen::Activity {
                    // Loaded again in the background rather than catching up
                    // with the backlog in the loop.
                    self.feed.load.refresh();
                }
                self.screen = Screen::Activity;
                return Action::None;
            }
            KeyCode::Char('H') => {
                if self.screen != Screen::History {
                    self.history.load.refresh();
                }
                self.screen = Screen::History;
                return Action::None;
            }
            _ => {}
        }
        match self.screen {
            Screen::Tasks if self.tab == Tab::Trace => {
                if let Some(action) = self.trace_key(key) {
                    return action;
                }
            }
            Screen::Tasks => {}
            Screen::Activity => return self.feed_key(key),
            Screen::History => return self.history_key(key),
        }
        let selected = self.selected_task().map(|t| t.id);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.tasks.len() {
                    self.selected += 1;
                }
                Action::Refresh
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::Refresh
            }
            KeyCode::Tab => {
                self.tab = self.tab.next();
                if self.tab == Tab::Trace {
                    self.detail.trace.load.refresh();
                }
                match (self.tab, selected) {
                    (Tab::Changes, Some(id)) => Action::LoadChanges(id),
                    _ => Action::None,
                }
            }
            KeyCode::Char('n') => {
                self.input = Some(Input {
                    kind: InputKind::NewTask,
                    buffer: String::new(),
                });
                Action::None
            }
            KeyCode::Char('r') => selected.map_or(Action::None, Action::Start),
            KeyCode::Char('R') => selected.map_or(Action::None, Action::Resume),
            KeyCode::Char('c') => selected.map_or(Action::None, Action::Cancel),
            KeyCode::Char('a') => match (selected, self.pending_approval()) {
                (Some(id), Some(_)) => Action::Approve(id),
                _ => {
                    self.say(Tone::Warn, "nothing to approve");
                    Action::None
                }
            },
            KeyCode::Char('x') => match (selected, self.pending_approval()) {
                (Some(id), Some(_)) => {
                    self.input = Some(Input {
                        kind: InputKind::Reject(id),
                        buffer: String::new(),
                    });
                    Action::None
                }
                _ => {
                    self.say(Tone::Warn, "nothing to reject");
                    Action::None
                }
            },
            KeyCode::Char('g') => Action::Refresh,
            _ => Action::None,
        }
    }

    /// Keys of the Activity screen.
    fn feed_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => self.screen = Screen::Tasks,
            KeyCode::Down | KeyCode::Char('j') => self.feed.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.feed.step(-1),
            KeyCode::PageDown => self.feed.step(10),
            KeyCode::PageUp => self.feed.step(-10),
            KeyCode::Char('f') => self.feed.toggle_follow(),
            KeyCode::Char('g') => self.feed.load.refresh(),
            KeyCode::Char(c @ '1'..='7') => {
                self.feed.toggle(c as usize - '0' as usize);
            }
            KeyCode::Enter => {
                if let Some(task) = self.feed.selected_line().map(|l| l.task) {
                    return self.focus_task(task);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Show `task` on the Tasks screen.
    pub fn focus_task(&mut self, task: TaskId) -> Action {
        match self.tasks.iter().position(|t| t.id == task) {
            Some(i) => {
                self.selected = i;
                self.screen = Screen::Tasks;
                Action::Refresh
            }
            None => {
                self.say(Tone::Warn, "this task is no longer on the board");
                Action::None
            }
        }
    }

    /// Keys of the History screen.
    fn history_key(&mut self, key: KeyEvent) -> Action {
        let h = &mut self.history;
        match key.code {
            KeyCode::Esc if h.open => h.open = false,
            KeyCode::Esc => self.screen = Screen::Tasks,
            KeyCode::Enter if h.selected_row().is_some() => {
                h.open = true;
                h.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') if h.open => h.scroll = h.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') if h.open => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::PageDown if h.open => h.scroll = h.scroll.saturating_add(10),
            KeyCode::PageUp if h.open => h.scroll = h.scroll.saturating_sub(10),
            KeyCode::Down | KeyCode::Char('j') => {
                if h.selected + 1 < h.rows.len() {
                    h.selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => h.selected = h.selected.saturating_sub(1),
            KeyCode::Char('a') => {
                h.all = !h.all;
                h.rows.clear();
                h.selected = 0;
                h.open = false;
                h.load = LoadState::Wanted;
            }
            KeyCode::Char('g') => h.load = LoadState::Wanted,
            _ => {}
        }
        Action::None
    }

    /// Keys of the Trace tab; `None` for the keys it leaves to the board.
    fn trace_key(&mut self, key: KeyEvent) -> Option<Action> {
        let t = &mut self.detail.trace;
        if let Some(output) = t.output.as_mut() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => output.scroll_by(1),
                KeyCode::Up | KeyCode::Char('k') => output.scroll_by(-1),
                KeyCode::PageDown => output.scroll_by(20),
                KeyCode::PageUp => output.scroll_by(-20),
                KeyCode::Esc => {
                    t.back();
                }
                KeyCode::Char('g') => {
                    t.output = None;
                    t.load = LoadState::Wanted;
                }
                KeyCode::Tab => {
                    // Leaving the tab closes the output.
                    t.output = None;
                    return None;
                }
                _ => return None,
            }
            return Some(Action::None);
        }
        match key.code {
            // Nothing open: stay on the tab (Esc only quits from the board).
            KeyCode::Esc => {
                t.back();
            }
            KeyCode::Down | KeyCode::Char('j') if t.expanded => {
                t.scroll = t.scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') if t.expanded => t.scroll = t.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => t.step(1),
            KeyCode::Up | KeyCode::Char('k') => t.step(-1),
            KeyCode::PageDown if !t.expanded => t.step(10),
            KeyCode::PageUp if !t.expanded => t.step(-10),
            KeyCode::Char('[') => t.switch_run(-1),
            KeyCode::Char(']') => t.switch_run(1),
            KeyCode::Enter => t.expand(),
            KeyCode::Char('o') => t.open_output(),
            KeyCode::Char('g') => t.load = LoadState::Wanted,
            _ => return None,
        }
        Some(Action::None)
    }

    fn next_request(&mut self) -> u64 {
        self.requests += 1;
        self.requests
    }

    /// Background loads the views shown need now; each is marked as
    /// loading, so it is asked for once.
    pub fn take_loads(&mut self) -> Vec<Load> {
        let mut loads = Vec::new();
        match self.screen {
            Screen::Activity if self.feed.load == LoadState::Wanted => {
                let id = self.next_request();
                self.feed.load = LoadState::Loading(id);
                loads.push(Load::Activity(id));
            }
            Screen::History if self.history.load == LoadState::Wanted => {
                let id = self.next_request();
                self.history.load = LoadState::Loading(id);
                loads.push(Load::History {
                    id,
                    all: self.history.all,
                });
            }
            Screen::Tasks if self.tab == Tab::Trace => {
                if let Some(task) = self.detail.task
                    && self.detail.trace.load == LoadState::Wanted
                {
                    let id = self.next_request();
                    self.detail.trace.load = LoadState::Loading(id);
                    loads.push(Load::Trace { id, task });
                }
                let wanted = self
                    .detail
                    .trace
                    .output
                    .as_ref()
                    .is_some_and(|o| o.load == LoadState::Wanted);
                if wanted && let Some(call) = self.detail.trace.selected_call().cloned() {
                    let id = self.next_request();
                    if let Some(o) = self.detail.trace.output.as_mut() {
                        o.load = LoadState::Loading(id);
                    }
                    loads.push(Load::Output {
                        id,
                        call: Box::new(call),
                    });
                }
            }
            _ => {}
        }
        loads
    }

    /// Take the first events of the Activity screen (answer to request
    /// `id`). `false` when the request is stale.
    pub fn set_feed(&mut self, id: u64, events: Result<Vec<TaggedEnvelope>, String>) -> bool {
        if !self.feed.load.awaits(id) {
            return false;
        }
        match events {
            Ok(events) => {
                let skip = events.len().saturating_sub(FEED_ON_ENTRY);
                self.feed.lines.clear();
                self.push_feed(&events[skip..]);
                self.feed.load = LoadState::Ready;
                true
            }
            Err(e) => {
                self.feed.load = LoadState::Failed(e);
                false
            }
        }
    }

    /// Add new events to the Activity screen.
    pub fn push_feed(&mut self, events: &[TaggedEnvelope]) {
        let lines: Vec<FeedLine> = events
            .iter()
            .filter_map(|t| {
                let group = Group::of(&t.envelope.event)?;
                let (tone, text) = self.describe_feed(&t.envelope.event)?;
                Some(FeedLine {
                    task: t.task,
                    number: t.number,
                    at: t.envelope.at,
                    group,
                    tone,
                    text,
                })
            })
            .collect();
        self.feed.push(lines);
    }

    /// Show an error reading the task index for the Activity screen, once
    /// until it changes or a poll succeeds (`None`).
    pub fn report_index_error(&mut self, error: Option<String>) {
        if let Some(e) = &error
            && self.feed.index_error.as_ref() != Some(e)
        {
            self.say(Tone::Bad, format!("cannot read the task index: {e}"));
        }
        self.feed.index_error = error;
    }

    /// Show a log read error of the Activity screen, once per task and
    /// message.
    pub fn report_feed_error(&mut self, task: TaskId, number: Option<u32>, error: &str) {
        if self.feed.first_report(task, error) {
            let label = number.map_or_else(|| task.short(), |n| format!("{n:03}"));
            self.say(
                Tone::Warn,
                format!("cannot read the log of task {label}: {error}"),
            );
        }
    }

    /// Take the History rows (answer to request `id`).
    pub fn set_history(&mut self, id: u64, rows: Result<Vec<TaskHistory>, String>) {
        if !self.history.load.awaits(id) {
            return;
        }
        match rows {
            Ok(rows) => {
                let h = &mut self.history;
                let current = h.selected_row().map(|r| r.task.id);
                h.rows = rows;
                h.selected = current
                    .and_then(|id| h.rows.iter().position(|r| r.task.id == id))
                    .unwrap_or(0);
                if h.rows.is_empty() {
                    h.open = false;
                }
                h.load = LoadState::Ready;
            }
            Err(e) => self.history.load = LoadState::Failed(e),
        }
    }

    /// Take the calls of the Trace tab (answer to request `id`).
    pub fn set_trace(&mut self, id: u64, data: Result<TraceData, String>) {
        let t = &mut self.detail.trace;
        if !t.load.awaits(id) {
            return;
        }
        match data {
            Ok(data) => t.set(data),
            Err(e) => t.load = LoadState::Failed(e),
        }
    }

    /// Take the complete output of a call (answer to request `id`).
    pub fn set_output(&mut self, id: u64, text: Result<Option<String>, String>) {
        let t = &mut self.detail.trace;
        let preview = t
            .selected_call()
            .map(|c| c.preview.clone())
            .unwrap_or_default();
        let Some(o) = t.output.as_mut() else {
            return;
        };
        if !o.load.awaits(id) {
            return;
        }
        match text {
            Ok(text) => o.set(text, &preview),
            Err(e) => o.load = LoadState::Failed(e),
        }
    }

    /// Show `task` in the detail pane, forgetting what was shown before when
    /// it is another task.
    pub fn select_detail(&mut self, task: TaskId) {
        if self.detail.task != Some(task) {
            self.detail = Detail {
                task: Some(task),
                ..Detail::default()
            };
        }
    }

    /// Forget the activity shown, before the log is read again from its
    /// start (another run).
    pub fn restart_log(&mut self) {
        self.detail.activity.clear();
        self.detail.streaming.clear();
        self.detail.budget = None;
    }

    /// Add events newly appended to the selected task's log.
    pub fn apply_log(&mut self, fresh: &[Envelope]) {
        let run = self.selected_run();
        for envelope in fresh {
            if run.is_some() && envelope.event.run_id() != run {
                continue;
            }
            self.apply(envelope);
        }
    }

    /// Add a live event: only streamed text of the selected run is used, the
    /// rest comes from the log.
    pub fn apply_live(&mut self, envelope: &Envelope) {
        if let Event::AgentDelta { run, delta, .. } = &envelope.event
            && Some(*run) == self.selected_run()
        {
            let text = match delta {
                StreamDelta::Text { text } | StreamDelta::Thinking { text } => text,
            };
            self.detail.streaming.push_str(text);
            let len = self.detail.streaming.chars().count();
            if len > STREAM_LIMIT {
                self.detail.streaming = self
                    .detail
                    .streaming
                    .chars()
                    .skip(len - STREAM_LIMIT)
                    .collect();
            }
        }
    }

    /// Title of a subtask of the selected task's plan, or its short id.
    #[must_use]
    pub fn subtask_title(&self, id: SubtaskId) -> String {
        self.detail
            .plan
            .as_ref()
            .and_then(|p| p.subtask(id))
            .map_or_else(|| id.short(), |s| s.title.clone())
    }

    fn apply(&mut self, envelope: &Envelope) {
        let event = &envelope.event;
        match event {
            Event::AgentText { .. } => self.detail.streaming.clear(),
            Event::BudgetUpdated {
                tokens,
                token_limit,
                active_ms,
                duration_limit_ms,
                ..
            } => {
                self.detail.budget = Some(Budget {
                    tokens: *tokens,
                    token_limit: *token_limit,
                    active_ms: *active_ms,
                    duration_limit_ms: *duration_limit_ms,
                });
            }
            _ => {}
        }
        if let Some((tone, text)) = self.describe(event) {
            self.detail.activity.push_back(Activity {
                seq: envelope.seq,
                tone,
                text,
            });
            while self.detail.activity.len() > ACTIVITY_LIMIT {
                self.detail.activity.pop_front();
            }
        }
    }

    /// One line of the Activity screen for `event`: [`App::describe`], and
    /// also commits, merges, budget updates and info logs.
    #[must_use]
    pub fn describe_feed(&self, event: &Event) -> Option<(Tone, String)> {
        if let Some(line) = self.describe(event) {
            return Some(line);
        }
        Some(match event {
            Event::Committed {
                commit,
                message,
                files,
                ..
            } => (
                Tone::Normal,
                format!(
                    "  ⎇ committed {} {} ({} file(s))",
                    short_sha(commit),
                    truncate(message, 120),
                    files.len()
                ),
            ),
            Event::Merged {
                commit,
                branch,
                base,
                ..
            } => (
                Tone::Good,
                format!("  ⎇ merged {branch} into {base} ({})", short_sha(commit)),
            ),
            Event::BudgetUpdated {
                tokens, active_ms, ..
            } => (
                Tone::Dim,
                format!(
                    "  budget: {} tokens, active {}",
                    human_tokens(*tokens),
                    human_duration(std::time::Duration::from_millis(*active_ms))
                ),
            ),
            Event::Log { message, .. } => (Tone::Dim, format!("  · {}", truncate(message, 200))),
            _ => return None,
        })
    }

    /// One activity line for `event`, or `None` when it is not shown.
    #[must_use]
    pub fn describe(&self, event: &Event) -> Option<(Tone, String)> {
        Some(match event {
            Event::RunStarted { .. } => (Tone::Normal, "▶ run started".into()),
            Event::PhaseStarted { phase, .. } => (Tone::Normal, format!("● {phase}")),
            Event::PhaseFinished {
                phase,
                success,
                summary,
                ..
            } => (
                if *success { Tone::Good } else { Tone::Warn },
                format!("  {phase} done: {}", truncate(summary, 160)),
            ),
            Event::AgentStarted { role, subtask, .. } => {
                let on =
                    subtask.map_or_else(String::new, |s| format!(" · {}", self.subtask_title(s)));
                (Tone::Dim, format!("  {role} started{on}"))
            }
            Event::AgentText { role, text, .. } => {
                let text = text.trim();
                if text.is_empty() {
                    return None;
                }
                (Tone::Normal, format!("  {role}: {}", truncate(text, 300)))
            }
            Event::ToolCalled { tool, input, .. } => {
                let args = serde_json::to_string(input).unwrap_or_default();
                (Tone::Dim, format!("    ⟶ {tool} {}", truncate(&args, 120)))
            }
            Event::ToolReturned {
                tool,
                is_error,
                preview,
                ..
            } if *is_error => (
                Tone::Warn,
                format!("    ⟵ {tool} failed: {}", truncate(preview.trim(), 120)),
            ),
            Event::AgentFinished { role, usage, .. } => (
                Tone::Dim,
                format!(
                    "  {role} finished ({} tokens)",
                    human_tokens(usage.input_tokens + usage.output_tokens)
                ),
            ),
            Event::SubtaskUpdated {
                subtask, status, ..
            } => (
                Tone::Normal,
                format!(
                    "  ▸ {}: {}",
                    self.subtask_title(*subtask),
                    enum_name(status)
                ),
            ),
            Event::SubtaskIntegrated {
                subtask, conflicts, ..
            } if !conflicts.is_empty() => (
                Tone::Warn,
                format!(
                    "  ▸ {}: conflicts in {}",
                    self.subtask_title(*subtask),
                    conflicts.join(", ")
                ),
            ),
            Event::ValidationFinished {
                command, passed, ..
            } => {
                if *passed {
                    (Tone::Good, format!("  ✓ {command}"))
                } else {
                    (Tone::Bad, format!("  ✗ {command}"))
                }
            }
            Event::ArtefactWritten { artefact, .. } => (
                Tone::Dim,
                match artefact {
                    Artefact::Spec => "  wrote the spec".to_string(),
                    Artefact::Plan => "  wrote the plan".to_string(),
                    Artefact::QaReport { round } => format!("  wrote QA report {round}"),
                },
            ),
            Event::ApprovalRequested { gate, .. } => (
                Tone::Warn,
                format!("⏸ approval needed: the {gate} (a: approve, x: reject)"),
            ),
            Event::ApprovalResolved { gate, approved, .. } => (
                Tone::Good,
                format!(
                    "  {gate} {}",
                    if *approved { "approved" } else { "rejected" }
                ),
            ),
            Event::Retrying { what, attempt, .. } => (
                Tone::Warn,
                format!("  ↻ retrying {} (attempt {attempt})", truncate(what, 120)),
            ),
            Event::Paused { reason, .. } => (Tone::Warn, format!("⏸ {}", truncate(reason, 200))),
            Event::RunFinished {
                status, success, ..
            } => (
                if *success { Tone::Good } else { Tone::Warn },
                format!("■ run finished: {}", status_name(*status)),
            ),
            Event::Log { level, message, .. } if level != "info" => (
                if level == "error" {
                    Tone::Bad
                } else {
                    Tone::Warn
                },
                format!("  ! {}", truncate(message, 200)),
            ),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::{Phase, RunId};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_tasks(n: usize) -> App {
        let mut app = App::default();
        let rows = (0..n)
            .map(|i| {
                let task = Task::new(format!("Task {i}"), "");
                TaskRow::new(&task, Some(u32::try_from(i).unwrap() + 1), false)
            })
            .collect();
        app.set_tasks(rows);
        app
    }

    #[test]
    fn navigation_and_run_keys() {
        let mut app = app_with_tasks(3);
        assert_eq!(app.handle_key(key(KeyCode::Down)), Action::Refresh);
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 2, "stays on the last row");
        app.handle_key(key(KeyCode::Up));
        let id = app.selected_task().unwrap().id;
        assert_eq!(app.handle_key(key(KeyCode::Char('r'))), Action::Start(id));
        assert_eq!(app.handle_key(key(KeyCode::Char('R'))), Action::Resume(id));
        assert_eq!(app.handle_key(key(KeyCode::Char('c'))), Action::Cancel(id));
        assert_eq!(app.handle_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.handle_key(key(KeyCode::Tab)), Action::LoadChanges(id));
        assert_eq!(app.tab, Tab::Changes);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn selection_follows_the_task_when_the_board_changes() {
        let mut app = app_with_tasks(3);
        app.selected = 2;
        let id = app.tasks[2].id;
        let mut rows = app.tasks.clone();
        rows.insert(0, TaskRow::new(&Task::new("New", ""), Some(9), false));
        app.set_tasks(rows);
        assert_eq!(app.selected_task().unwrap().id, id);
        app.set_tasks(Vec::new());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn new_task_input() {
        let mut app = app_with_tasks(0);
        app.handle_key(key(KeyCode::Char('n')));
        for c in "Add x".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::NewTask("Add y".into())
        );
        assert!(app.input.is_none());
        // Empty titles and Esc do nothing.
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(app.input.is_none());
    }

    #[test]
    fn approvals_need_a_pending_gate_and_a_reason_to_reject() {
        let mut app = app_with_tasks(1);
        let id = app.tasks[0].id;
        assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Action::None);
        let mut state = RunState::new(RunId::new(), id, Phase::Build);
        state.pending_approval = Some(ApprovalGate::Plan);
        app.select_detail(id);
        app.detail.state = Some(state);
        assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Action::Approve(id));
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.input.as_ref().unwrap().kind, InputKind::Reject(id));
        for c in "too big".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Reject(id, "too big".into())
        );
    }

    #[test]
    fn log_and_live_events_build_the_activity() {
        let mut app = app_with_tasks(1);
        let id = app.tasks[0].id;
        let run = RunId::new();
        app.select_detail(id);
        app.detail.state = Some(RunState::new(run, id, Phase::Plan));
        let other = RunId::new();
        let mut log = vec![
            Envelope::now(Event::PhaseStarted {
                run,
                phase: Phase::Plan,
            }),
            Envelope::now(Event::PhaseStarted {
                run: other,
                phase: Phase::Qa,
            }),
        ];
        app.apply_log(&log);
        assert_eq!(app.detail.activity.len(), 1, "other runs are ignored");
        app.apply_live(&Envelope::now(Event::AgentDelta {
            run,
            role: vibe_core::AgentRole::Planner,
            subtask: None,
            delta: StreamDelta::Text {
                text: "Thinking about".into(),
            },
        }));
        assert_eq!(app.detail.streaming, "Thinking about");
        log.push(Envelope::now(Event::AgentText {
            run,
            role: vibe_core::AgentRole::Planner,
            text: "Thinking about it.".into(),
        }));
        log.push(Envelope::now(Event::BudgetUpdated {
            run,
            tokens: 1200,
            token_limit: Some(10_000),
            active_ms: 5_000,
            duration_limit_ms: None,
        }));
        // Only what was appended is given (the reader keeps the offset).
        app.apply_log(&log[2..]);
        assert!(app.detail.streaming.is_empty(), "the full text replaces it");
        assert_eq!(app.detail.activity.len(), 2);
        assert_eq!(app.detail.budget.unwrap().tokens, 1200);
    }

    fn tagged(task: &TaskRow, event: Event) -> TaggedEnvelope {
        TaggedEnvelope {
            task: task.id,
            number: task.number.unwrap(),
            envelope: Envelope::now(event),
        }
    }

    #[test]
    fn screens_switch_with_keys_and_esc_goes_back_before_quitting() {
        let mut app = app_with_tasks(2);
        assert_eq!(app.handle_key(key(KeyCode::Char('A'))), Action::None);
        assert_eq!(app.screen, Screen::Activity);
        assert_eq!(app.handle_key(key(KeyCode::Char('H'))), Action::None);
        assert_eq!(app.screen, Screen::History);
        // `a` toggles failed/cancelled tasks here, it does not approve.
        app.history.load = LoadState::Ready;
        assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Action::None);
        assert!(app.history.all);
        assert_eq!(app.history.load, LoadState::Wanted);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.screen, Screen::Tasks);
        app.handle_key(key(KeyCode::Char('A')));
        assert_eq!(app.handle_key(key(KeyCode::Char('T'))), Action::Refresh);
        assert_eq!(app.screen, Screen::Tasks);
        app.handle_key(key(KeyCode::Char('H')));
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::Quit);
        app.screen = Screen::Tasks;
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn background_loads_are_asked_once_and_stale_answers_dropped() {
        let mut app = app_with_tasks(1);
        assert!(app.take_loads().is_empty(), "the board needs none");
        app.handle_key(key(KeyCode::Char('A')));
        let loads = app.take_loads();
        let [Load::Activity(first)] = loads[..] else {
            panic!("{loads:?}")
        };
        assert!(app.take_loads().is_empty(), "asked once");
        assert!(!app.set_feed(first + 1, Ok(Vec::new())), "not this request");
        let row = app.tasks[0].clone();
        let run = RunId::new();
        let events: Vec<TaggedEnvelope> = (0..250)
            .map(|_| {
                tagged(
                    &row,
                    Event::PhaseStarted {
                        run,
                        phase: Phase::Build,
                    },
                )
            })
            .collect();
        assert!(app.set_feed(first, Ok(events)));
        assert_eq!(app.feed.lines.len(), FEED_ON_ENTRY);
        assert_eq!(app.feed.load, LoadState::Ready);

        app.handle_key(key(KeyCode::Char('H')));
        let loads = app.take_loads();
        let [Load::History { id, all: false }] = loads[..] else {
            panic!("{loads:?}")
        };
        app.handle_key(key(KeyCode::Char('g')));
        let [Load::History { id: again, .. }] = app.take_loads()[..] else {
            panic!("g asks again")
        };
        app.set_history(id, Err("late".into()));
        assert_eq!(
            app.history.load,
            LoadState::Loading(again),
            "the old answer is dropped"
        );
        app.set_history(again, Ok(Vec::new()));
        assert_eq!(app.history.load, LoadState::Ready);
        // Coming back to the screen loads it again.
        app.handle_key(key(KeyCode::Char('T')));
        app.handle_key(key(KeyCode::Char('H')));
        assert!(matches!(app.take_loads()[..], [Load::History { .. }]));
    }

    #[test]
    fn activity_filters_follow_and_jump_to_the_task() {
        let mut app = app_with_tasks(2);
        let (one, two) = (app.tasks[0].clone(), app.tasks[1].clone());
        let run = RunId::new();
        app.screen = Screen::Activity;
        app.push_feed(&[
            tagged(
                &one,
                Event::PhaseStarted {
                    run,
                    phase: Phase::Build,
                },
            ),
            tagged(
                &two,
                Event::ToolCalled {
                    run,
                    role: vibe_core::AgentRole::Coder,
                    tool: "read_file".into(),
                    input: serde_json::json!({}),
                    call: vibe_core::CallId::new(),
                    subtask: None,
                },
            ),
            tagged(
                &one,
                Event::Committed {
                    run,
                    subtask: None,
                    commit: "abcdef0123".into(),
                    message: "add x".into(),
                    files: vec!["x".into()],
                },
            ),
            tagged(
                &two,
                Event::BudgetUpdated {
                    run,
                    tokens: 10,
                    token_limit: None,
                    active_ms: 0,
                    duration_limit_ms: None,
                },
            ),
        ]);
        assert_eq!(app.feed.visible().len(), 4);
        assert!(app.feed.lines[2].text.contains("committed abcdef01 add x"));
        assert_eq!(app.feed.lines[0].label(), "#001");
        assert_eq!(app.feed.selection(), Some(3), "follows the newest");

        app.handle_key(key(KeyCode::Char('2')));
        assert!(!app.feed.shows(Group::Tools));
        assert_eq!(app.feed.visible().len(), 3);
        app.handle_key(key(KeyCode::Char('6')));
        assert_eq!(app.feed.visible().len(), 2);
        app.handle_key(key(KeyCode::Char('2')));
        assert_eq!(app.feed.visible().len(), 3, "toggled back on");

        app.handle_key(key(KeyCode::Up));
        assert!(!app.feed.follow, "moving stops following");
        assert_eq!(app.feed.selected_line().unwrap().task, two.id);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.feed.selected_line().unwrap().task, one.id);
        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.feed.follow);
        app.handle_key(key(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Refresh);
        assert_eq!(app.screen, Screen::Tasks);
        assert_eq!(app.selected_task().unwrap().id, two.id);
    }

    #[test]
    fn activity_is_loaded_again_on_entry_on_g_and_after_a_failure() {
        let mut app = app_with_tasks(1);
        app.handle_key(key(KeyCode::Char('A')));
        let [Load::Activity(first)] = app.take_loads()[..] else {
            panic!("first entry")
        };
        app.set_feed(first, Err("index unreadable".into()));
        assert!(matches!(app.feed.load, LoadState::Failed(_)));
        // g retries.
        app.handle_key(key(KeyCode::Char('g')));
        let [Load::Activity(second)] = app.take_loads()[..] else {
            panic!("g retries")
        };
        assert!(app.set_feed(second, Ok(Vec::new())));
        // Leaving and coming back loads again, in the background.
        app.handle_key(key(KeyCode::Char('T')));
        assert!(app.take_loads().is_empty());
        app.handle_key(key(KeyCode::Char('A')));
        assert!(matches!(app.take_loads()[..], [Load::Activity(_)]));
        // A while already there does not.
        app.handle_key(key(KeyCode::Char('A')));
        assert!(app.take_loads().is_empty());
    }

    #[test]
    fn index_errors_are_shown_once_until_they_change() {
        let mut app = app_with_tasks(0);
        app.report_index_error(Some("gone".into()));
        assert!(app.message.take().unwrap().1.contains("gone"));
        app.report_index_error(Some("gone".into()));
        assert!(app.message.is_none(), "not again at the next tick");
        app.report_index_error(None);
        app.report_index_error(Some("gone".into()));
        assert!(app.message.is_some(), "again after a successful poll");
    }

    #[test]
    fn feed_read_errors_are_shown_once() {
        let mut app = app_with_tasks(1);
        let id = app.tasks[0].id;
        app.report_feed_error(id, Some(1), "Is a directory");
        assert_eq!(
            app.message.take().unwrap().1,
            "cannot read the log of task 001: Is a directory"
        );
        app.report_feed_error(id, Some(1), "Is a directory");
        assert!(app.message.is_none());
    }

    #[test]
    fn the_trace_tab_expands_a_call_and_loads_its_output() {
        let mut app = app_with_tasks(2);
        let id = app.tasks[0].id;
        app.select_detail(id);
        app.tab = Tab::Changes;
        assert_eq!(app.handle_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.tab, Tab::Trace);
        let loads = app.take_loads();
        let [Load::Trace { id: request, task }] = loads[..] else {
            panic!("{loads:?}")
        };
        assert_eq!(task, id);
        let run = RunId::new();
        let mut log = vec![Envelope::now(Event::RunStarted { run, task: id })];
        log.extend(crate::tui::panes::tests::call(
            run,
            1,
            "read_file",
            serde_json::json!({"path": "a"}),
        ));
        log.extend(crate::tui::panes::tests::call(
            run,
            3,
            "bash",
            serde_json::json!({"command": "ls"}),
        ));
        app.set_trace(
            request,
            Ok(TraceData::from_events(&log, std::path::Path::new("/p"))),
        );
        assert_eq!(app.detail.trace.calls().len(), 2);

        // ↑↓ move among the calls, not the tasks.
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.selected, 0);
        assert_eq!(app.detail.trace.selected, 1);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.detail.trace.expanded);
        app.handle_key(key(KeyCode::Char('o')));
        let loads = app.take_loads();
        let [Load::Output { id: out, ref call }] = loads[..] else {
            panic!("{loads:?}")
        };
        assert_eq!(call.tool, "bash");
        app.set_output(out, Ok(Some("line 1\nline 2\nline 3".into())));
        let output = app.detail.trace.output.as_ref().unwrap();
        assert_eq!(output.text.as_deref(), Some("line 1\nline 2\nline 3"));
        assert_eq!(output.lines.len(), 3, "split once");
        for _ in 0..5 {
            app.handle_key(key(KeyCode::Down));
        }
        assert_eq!(
            app.detail.trace.output.as_ref().unwrap().scroll,
            2,
            "stops at the last line"
        );

        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(app.detail.trace.output.is_none() && app.detail.trace.expanded);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(!app.detail.trace.expanded);
        // Nothing open: Esc stays on the tab instead of quitting.
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(app.tab, Tab::Trace);
        // Other keys still act on the task.
        assert_eq!(app.handle_key(key(KeyCode::Char('r'))), Action::Start(id));

        // With the output open, g reloads the calls and Tab closes it.
        app.detail.trace.load = LoadState::Ready;
        app.handle_key(key(KeyCode::Char('o')));
        app.take_loads();
        assert_eq!(app.handle_key(key(KeyCode::Char('g'))), Action::None);
        assert!(app.detail.trace.output.is_none());
        assert!(matches!(app.take_loads()[..], [Load::Trace { .. }]));
        app.detail.trace.load = LoadState::Ready;
        app.handle_key(key(KeyCode::Char('o')));
        app.take_loads();
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Activity);
        assert!(app.detail.trace.output.is_none());
        for _ in 0..2 {
            app.handle_key(key(KeyCode::Tab));
        }
        assert_eq!(app.tab, Tab::Changes);
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::Quit);

        // Coming back to the tab loads the calls again.
        for _ in 0..1 {
            app.handle_key(key(KeyCode::Tab));
        }
        assert_eq!(app.tab, Tab::Trace);
        assert!(matches!(app.take_loads()[..], [Load::Trace { .. }]));

        // Another task forgets the trace, which is loaded again.
        app.select_detail(app.tasks[1].id);
        assert!(matches!(app.take_loads()[..], [Load::Trace { .. }]));
    }
}
