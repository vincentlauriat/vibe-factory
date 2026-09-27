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
use vibe_pipeline::RunState;

use crate::util::{enum_name, human_tokens, status_name, truncate};

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
}

impl Tab {
    /// Every tab, in display order.
    pub const ALL: [Tab; 3] = [Tab::Activity, Tab::Plan, Tab::Changes];

    /// Title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Tab::Activity => "Activity",
            Tab::Plan => "Plan",
            Tab::Changes => "Changes",
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::Activity => Tab::Plan,
            Tab::Plan => Tab::Changes,
            Tab::Changes => Tab::Activity,
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
    /// Log entries already turned into activity.
    pub consumed: usize,
    /// Text of the step being streamed.
    pub streaming: String,
    /// Budget, from the last `budget_updated`.
    pub budget: Option<Budget>,
    /// Changes of the workspace, once loaded.
    pub changes: Option<String>,
}

/// The whole UI state.
#[derive(Debug, Clone, Default)]
pub struct App {
    /// Project name, for the title.
    pub project: String,
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

    /// Add the logged events not seen yet (`log` is the whole event log of
    /// the selected task).
    pub fn apply_log(&mut self, log: &[Envelope]) {
        let run = self.selected_run();
        let fresh: Vec<Envelope> = log.iter().skip(self.detail.consumed).cloned().collect();
        self.detail.consumed = log.len();
        for envelope in fresh {
            if run.is_some() && envelope.event.run_id() != run {
                continue;
            }
            self.apply(&envelope);
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

    fn subtask_title(&self, id: SubtaskId) -> String {
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
        app.apply_log(&log);
        assert!(app.detail.streaming.is_empty(), "the full text replaces it");
        assert_eq!(app.detail.activity.len(), 2);
        assert_eq!(app.detail.budget.unwrap().tokens, 1200);
        // Seeing the same log again adds nothing.
        app.apply_log(&log);
        assert_eq!(app.detail.activity.len(), 2);
    }
}
