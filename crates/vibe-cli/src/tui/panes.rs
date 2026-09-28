//! State of the Activity and History screens and of the Trace tab.
//!
//! Like [`super::app`], everything here is free of I/O: the data these views
//! show is loaded in the background by the event loop, which hands it over
//! with a request id so that a late answer to an older request is dropped.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use console::measure_text_width;
use vibe_core::{Envelope, Event, RunId, TaskId};
use vibe_pipeline::history::{ChangedFilesSource, FileStatus, RunSummaryState, TaskHistory};
use vibe_pipeline::trace::{self, Call, PairedBy};

use super::app::Tone;
use crate::util::{
    cost_text, enum_name, files_count, finished_at, human_duration, human_tokens, lower_bound,
    relative_time_from, short_sha, status_name, truncate,
};

/// Events kept by the Activity screen.
pub const FEED_LIMIT: usize = 2000;

/// Events shown when the Activity screen is opened.
pub const FEED_ON_ENTRY: usize = 200;

/// Where data loaded in the background stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    /// To be loaded (first time, or again after `g`).
    #[default]
    Wanted,
    /// Requested; only the answer to this request id is taken.
    Loading(u64),
    /// Loaded.
    Ready,
    /// Loading failed.
    Failed(String),
}

impl LoadState {
    /// Whether the answer to request `id` is awaited.
    #[must_use]
    pub fn awaits(&self, id: u64) -> bool {
        *self == LoadState::Loading(id)
    }

    /// Ask for fresh data (on entering a view), unless a load is running.
    pub fn refresh(&mut self) {
        if !matches!(self, LoadState::Loading(_)) {
            *self = LoadState::Wanted;
        }
    }
}

/// Groups of events that the Activity screen can hide, toggled with the
/// keys `1` to `7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Agent sessions, their text and retries.
    Agents,
    /// Tool calls.
    Tools,
    /// Runs, phases, subtasks, validations and artefacts.
    Phases,
    /// Commits, integrations and merges.
    Git,
    /// Approval requests and decisions.
    Approvals,
    /// Budget updates.
    Budget,
    /// Log messages.
    Logs,
}

impl Group {
    /// Every group, in key order.
    pub const ALL: [Group; 7] = [
        Group::Agents,
        Group::Tools,
        Group::Phases,
        Group::Git,
        Group::Approvals,
        Group::Budget,
        Group::Logs,
    ];

    /// Name shown in the filter bar.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Group::Agents => "agents",
            Group::Tools => "tools",
            Group::Phases => "phases",
            Group::Git => "git",
            Group::Approvals => "approvals",
            Group::Budget => "budget",
            Group::Logs => "logs",
        }
    }

    fn index(self) -> usize {
        Group::ALL.iter().position(|g| *g == self).unwrap_or(0)
    }

    /// Group of `event`; `None` for streamed deltas, which are not logged.
    #[must_use]
    pub fn of(event: &Event) -> Option<Group> {
        Some(match event {
            Event::AgentStarted { .. }
            | Event::AgentText { .. }
            | Event::AgentFinished { .. }
            | Event::Retrying { .. } => Group::Agents,
            Event::ToolCalled { .. } | Event::ToolReturned { .. } => Group::Tools,
            Event::RunStarted { .. }
            | Event::PhaseStarted { .. }
            | Event::PhaseFinished { .. }
            | Event::SubtaskUpdated { .. }
            | Event::ValidationFinished { .. }
            | Event::ArtefactWritten { .. }
            | Event::Paused { .. }
            | Event::RunFinished { .. } => Group::Phases,
            Event::SubtaskIntegrated { .. } | Event::Committed { .. } | Event::Merged { .. } => {
                Group::Git
            }
            Event::ApprovalRequested { .. } | Event::ApprovalResolved { .. } => Group::Approvals,
            Event::BudgetUpdated { .. } => Group::Budget,
            Event::Log { .. } => Group::Logs,
            Event::AgentDelta { .. } => return None,
        })
    }
}

/// One line of the Activity screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLine {
    /// Task the event belongs to.
    pub task: TaskId,
    /// Its number.
    pub number: u32,
    /// When it happened.
    pub at: DateTime<Utc>,
    /// Its group.
    pub group: Group,
    /// How to show it.
    pub tone: Tone,
    /// Text.
    pub text: String,
}

impl FeedLine {
    /// `#003`, the prefix of the line.
    #[must_use]
    pub fn label(&self) -> String {
        format!("#{:03}", self.number)
    }
}

/// The Activity screen: events of every task, newest last.
#[derive(Debug, Clone)]
pub struct Feed {
    /// Loading of the first events.
    pub load: LoadState,
    /// Lines, oldest first.
    pub lines: VecDeque<FeedLine>,
    /// Hidden groups, by [`Group::ALL`] index.
    pub hidden: [bool; 7],
    /// Whether the selection stays on the newest line.
    pub follow: bool,
    /// Selected line, among the visible ones (when not following).
    pub selected: usize,
    /// Read errors already shown, per task.
    pub reported: HashSet<(TaskId, String)>,
    /// Last error reading the task index, already shown.
    pub index_error: Option<String>,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            load: LoadState::Wanted,
            lines: VecDeque::new(),
            hidden: [false; 7],
            follow: true,
            selected: 0,
            reported: HashSet::new(),
            index_error: None,
        }
    }
}

impl Feed {
    /// Whether events of `group` are shown.
    #[must_use]
    pub fn shows(&self, group: Group) -> bool {
        !self.hidden[group.index()]
    }

    /// Lines of the shown groups, oldest first.
    #[must_use]
    pub fn visible(&self) -> Vec<&FeedLine> {
        self.lines.iter().filter(|l| self.shows(l.group)).collect()
    }

    /// Index of the selected line among [`Feed::visible`].
    #[must_use]
    pub fn selection(&self) -> Option<usize> {
        let count = self.visible().len();
        if count == 0 {
            None
        } else if self.follow {
            Some(count - 1)
        } else {
            Some(self.selected.min(count - 1))
        }
    }

    /// The selected line.
    #[must_use]
    pub fn selected_line(&self) -> Option<&FeedLine> {
        let visible = self.visible();
        self.selection().map(|i| visible[i])
    }

    /// Add lines, dropping the oldest beyond [`FEED_LIMIT`].
    pub fn push(&mut self, lines: impl IntoIterator<Item = FeedLine>) {
        self.lines.extend(lines);
        let mut dropped_visible = 0;
        while self.lines.len() > FEED_LIMIT {
            if let Some(line) = self.lines.pop_front()
                && self.shows(line.group)
            {
                dropped_visible += 1;
            }
        }
        self.selected = self.selected.saturating_sub(dropped_visible);
    }

    /// Show or hide the group of key `n` (1 to 7).
    pub fn toggle(&mut self, n: usize) {
        if let Some(group) = n.checked_sub(1).and_then(|i| Group::ALL.get(i)) {
            let current = self.selection();
            self.hidden[group.index()] = !self.hidden[group.index()];
            if let Some(i) = current {
                self.selected = i.min(self.visible().len().saturating_sub(1));
            }
        }
    }

    /// Move the selection by `delta` lines; it stops following.
    pub fn step(&mut self, delta: isize) {
        let Some(current) = self.selection() else {
            return;
        };
        self.follow = false;
        let last = self.visible().len() - 1;
        self.selected = current.saturating_add_signed(delta).min(last);
    }

    /// Follow the newest line, or stop following it.
    pub fn toggle_follow(&mut self) {
        if self.follow {
            self.selected = self.selection().unwrap_or(0);
        }
        self.follow = !self.follow;
    }

    /// Whether this read error of `task` is new (and remember it).
    pub fn first_report(&mut self, task: TaskId, error: &str) -> bool {
        self.reported.insert((task, error.to_string()))
    }
}

/// The History screen.
#[derive(Debug, Clone, Default)]
pub struct HistoryView {
    /// Loading of the rows.
    pub load: LoadState,
    /// Whether failed and cancelled tasks are listed too.
    pub all: bool,
    /// Tasks, most recent activity first.
    pub rows: Vec<TaskHistory>,
    /// Selected row.
    pub selected: usize,
    /// Whether the detail of the selected task is open.
    pub open: bool,
    /// Scroll of the detail.
    pub scroll: u16,
}

impl HistoryView {
    /// The selected task.
    #[must_use]
    pub fn selected_row(&self) -> Option<&TaskHistory> {
        self.rows.get(self.selected)
    }
}

/// Columns of the History table.
pub const HISTORY_COLUMNS: [&str; 10] = [
    "#", "title", "status", "runs", "commits", "files", "tokens", "active", "cost", "finished",
];

/// The cells of one History row, in the notation of `vibe history`: `+`
/// marks a lower bound, `~` an approximate file count. `now` is the
/// reference of relative times.
#[must_use]
pub fn history_row(h: &TaskHistory, now: DateTime<Utc>) -> [String; 10] {
    let tokens = h.totals.usage.input_tokens + h.totals.usage.output_tokens;
    [
        format!("{:03}", h.number),
        h.task.title.clone(),
        status_name(h.task.status).to_string(),
        h.totals.runs.to_string(),
        h.totals.commits.to_string(),
        files_count(h),
        lower_bound(human_tokens(tokens), h.totals.complete),
        lower_bound(
            human_duration(Duration::from_millis(h.totals.active_ms)),
            h.totals.complete,
        ),
        h.cost.as_ref().map_or_else(|| "-".to_string(), cost_text),
        relative_time_from(finished_at(h), now),
    ]
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn file_status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Copied => "C",
        FileStatus::Unknown => "?",
    }
}

/// The lines of the detail of one History row.
#[must_use]
pub fn history_detail(h: &TaskHistory) -> Vec<(Tone, String)> {
    let mut out = vec![(
        Tone::Normal,
        format!(
            "#{:03} {} · {}",
            h.number,
            h.task.title,
            status_name(h.task.status)
        ),
    )];
    let section = |out: &mut Vec<(Tone, String)>, title: String| {
        out.push((Tone::Normal, String::new()));
        out.push((Tone::Normal, title));
    };

    section(&mut out, format!("Runs ({})", h.runs.len()));
    for r in &h.runs {
        let state = match r.state {
            RunSummaryState::Running => "running".to_string(),
            RunSummaryState::Interrupted => "interrupted".to_string(),
            RunSummaryState::Finished => r
                .status
                .map_or_else(|| "finished".to_string(), |s| status_name(s).to_string()),
        };
        let duration = r.finished_at.map_or_else(
            || "-".to_string(),
            |end| human_duration((end - r.started_at).to_std().unwrap_or_default()),
        );
        let tokens = human_tokens(r.usage.input_tokens + r.usage.output_tokens);
        let known = if r.totals_known {
            ""
        } else {
            " (totals unknown)"
        };
        let resumes = if r.resumes > 0 {
            format!(" · {} resume(s)", r.resumes)
        } else {
            String::new()
        };
        let tone = match (r.state, r.success) {
            (RunSummaryState::Interrupted, _) | (_, Some(false)) => Tone::Warn,
            (_, Some(true)) => Tone::Good,
            _ => Tone::Normal,
        };
        out.push((
            tone,
            format!(
                "  {state:<11} started {} · {duration} · {tokens} tokens{known}{resumes}",
                local_time(r.started_at)
            ),
        ));
    }

    let commits: Vec<_> = h.runs.iter().flat_map(|r| &r.commits).collect();
    section(&mut out, format!("Commits ({})", commits.len()));
    for c in commits {
        let message = if c.message.is_empty() {
            "(no message recorded)".to_string()
        } else {
            truncate(&c.message, 80)
        };
        out.push((
            Tone::Normal,
            format!(
                "  {} {message} ({} file(s))",
                short_sha(&c.commit),
                c.files.len()
            ),
        ));
    }
    for r in &h.runs {
        if let Some(m) = &r.merged {
            out.push((
                Tone::Good,
                format!(
                    "  merged {} into {} ({})",
                    m.branch,
                    m.base,
                    short_sha(&m.commit)
                ),
            ));
        }
    }

    let files = &h.changed_files;
    let source = match &files.source {
        ChangedFilesSource::Branch { branch, base } => format!("branch {branch} against {base}"),
        ChangedFilesSource::MergeCommit {
            commit,
            fast_forward,
        } => format!(
            "merge {}{}",
            short_sha(commit),
            if *fast_forward { " (fast-forward)" } else { "" }
        ),
        ChangedFilesSource::Commits => "commit events".to_string(),
        ChangedFilesSource::Trace => "tool trace".to_string(),
        ChangedFilesSource::None => "nothing recorded".to_string(),
    };
    let approximate = if files.approximate {
        ", approximate"
    } else {
        ""
    };
    section(
        &mut out,
        format!(
            "Changed files ({}) · from {source}{approximate}",
            files.files.len()
        ),
    );
    for f in &files.files {
        let from = f
            .old_path
            .as_ref()
            .map_or_else(String::new, |p| format!(" (from {p})"));
        out.push((
            Tone::Normal,
            format!("  {} {}{from}", file_status_letter(f.status), f.path),
        ));
    }

    let validations: Vec<_> = h.runs.iter().flat_map(|r| &r.validations).collect();
    section(&mut out, format!("Validations ({})", validations.len()));
    for v in validations {
        let exit = v
            .exit_code
            .map_or_else(String::new, |c| format!(" (exit {c})"));
        out.push(if v.passed {
            (Tone::Good, format!("  ✓ {}", v.command))
        } else {
            (Tone::Bad, format!("  ✗ {}{exit}", v.command))
        });
    }

    section(&mut out, "Last QA".to_string());
    match &h.last_qa {
        Some(qa) => out.push((
            Tone::Normal,
            format!(
                "  round {}: {} · {} ({} issue(s))",
                qa.round,
                enum_name(&qa.verdict),
                truncate(&qa.summary, 120),
                qa.issues
            ),
        )),
        None => out.push((Tone::Dim, "  none".to_string())),
    }

    let mut errors: Vec<String> = h.runs.iter().filter_map(|r| r.last_error.clone()).collect();
    errors.extend(h.errors.iter().cloned());
    if !errors.is_empty() {
        section(&mut out, format!("Errors ({})", errors.len()));
        for e in errors {
            out.push((Tone::Bad, format!("  {}", truncate(&e, 200))));
        }
    }
    out
}

/// Tool calls of a task, as the Trace tab loads them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TraceData {
    /// Runs of the task, oldest first.
    pub runs: Vec<RunId>,
    /// Calls of every run.
    pub calls: Vec<Call>,
    /// The run shown first (the last one).
    pub last: Option<RunId>,
}

impl TraceData {
    /// Pair the calls of `events`, the log of a task of the project at
    /// `project_root`.
    #[must_use]
    pub fn from_events(events: &[Envelope], project_root: &Path) -> Self {
        let mut runs = Vec::new();
        for run in events.iter().filter_map(|e| e.event.run_id()) {
            if !runs.contains(&run) {
                runs.push(run);
            }
        }
        Self {
            runs,
            calls: trace::pair_all_calls(events, project_root),
            last: trace::last_run(events),
        }
    }
}

/// The complete output of a call, opened with `o`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputView {
    /// Loading of the output.
    pub load: LoadState,
    /// The output; `None` when the trace store does not have it.
    pub text: Option<String>,
    /// Lines shown: the output, or the preview when it is missing (split
    /// once, not at every frame).
    pub lines: Vec<String>,
    /// First line shown.
    pub scroll: usize,
}

impl OutputView {
    /// Take the loaded output; `preview` stands in when it is missing.
    pub fn set(&mut self, text: Option<String>, preview: &str) {
        self.lines = text
            .as_deref()
            .unwrap_or(preview)
            .lines()
            .map(str::to_string)
            .collect();
        self.text = text;
        self.scroll = 0;
        self.load = LoadState::Ready;
    }

    /// Scroll by `delta` lines, never past the last one.
    pub fn scroll_by(&mut self, delta: isize) {
        let last = self.lines.len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(delta).min(last);
    }
}

/// The Trace tab of the selected task.
#[derive(Debug, Clone, Default)]
pub struct TraceView {
    /// Loading of the calls.
    pub load: LoadState,
    /// Loaded calls.
    pub data: TraceData,
    /// Index of the run shown in `data.runs`.
    pub run: usize,
    /// Selected call among the calls of the run shown.
    pub selected: usize,
    /// Whether the selected call is expanded.
    pub expanded: bool,
    /// Scroll of the expanded call, in lines.
    pub scroll: u16,
    /// Complete output of the selected call, once asked for.
    pub output: Option<OutputView>,
}

impl TraceView {
    /// Take loaded calls, keeping the run shown when it is still there.
    pub fn set(&mut self, data: TraceData) {
        let current = self.current_run();
        self.run = current
            .or(data.last)
            .and_then(|r| data.runs.iter().position(|x| *x == r))
            .unwrap_or_else(|| data.runs.len().saturating_sub(1));
        self.data = data;
        self.selected = self.selected.min(self.calls().len().saturating_sub(1));
        self.load = LoadState::Ready;
    }

    /// Run shown.
    #[must_use]
    pub fn current_run(&self) -> Option<RunId> {
        self.data.runs.get(self.run).copied()
    }

    /// Calls of the run shown, in call order.
    #[must_use]
    pub fn calls(&self) -> Vec<&Call> {
        let run = self.current_run();
        self.data
            .calls
            .iter()
            .filter(|c| Some(c.run) == run)
            .collect()
    }

    /// The selected call.
    #[must_use]
    pub fn selected_call(&self) -> Option<&Call> {
        self.calls().get(self.selected).copied()
    }

    /// Show the previous (`-1`) or next (`1`) run.
    pub fn switch_run(&mut self, delta: isize) {
        let last = self.data.runs.len().saturating_sub(1);
        let run = self.run.saturating_add_signed(delta).min(last);
        if run != self.run {
            self.run = run;
            self.selected = 0;
            self.collapse();
        }
    }

    /// Move the selection by `delta` calls.
    pub fn step(&mut self, delta: isize) {
        let last = self.calls().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// Expand the selected call.
    pub fn expand(&mut self) {
        if self.selected_call().is_some() {
            self.expanded = true;
            self.scroll = 0;
        }
    }

    /// Close the output, or else the expanded call. `false` when neither
    /// was open.
    pub fn back(&mut self) -> bool {
        if self.output.take().is_some() {
            return true;
        }
        let was = self.expanded;
        self.collapse();
        was
    }

    fn collapse(&mut self) {
        self.expanded = false;
        self.output = None;
        self.scroll = 0;
    }

    /// Ask for the complete output of the selected call.
    pub fn open_output(&mut self) {
        if self.selected_call().is_some() {
            self.expanded = true;
            self.output = Some(OutputView::default());
        }
    }
}

/// One row of the Trace list: `seq role subtask tool duration exit err`.
#[must_use]
pub fn call_row(seq: usize, call: &Call, subtask: &str) -> String {
    let duration = call.duration_ms.map_or_else(
        || "-".to_string(),
        |ms| human_duration(Duration::from_millis(ms)),
    );
    let exit = call
        .exit_code
        .map_or_else(|| "-".to_string(), |c| c.to_string());
    let err = match (call.is_error, call.timed_out) {
        (_, true) => "timeout",
        (Some(true), _) => "error",
        (None, _) => "no result",
        _ => "",
    };
    format!(
        "{seq:>4} {} {} {} {duration:>9} {exit:>4} {err}",
        fit(&call.role.to_string(), 9),
        fit(subtask, 16),
        fit(&call.tool, 14)
    )
}

/// `text` cut and padded to exactly `width` terminal columns (wide
/// characters such as CJK or emoji count for two).
#[must_use]
pub fn fit(text: &str, width: usize) -> String {
    let flat = truncate(text, usize::MAX);
    let mut out = String::new();
    let mut used = 0;
    let fits = measure_text_width(&flat) <= width;
    let room = if fits { width } else { width.saturating_sub(1) };
    for c in flat.chars() {
        let w = measure_text_width(c.encode_utf8(&mut [0; 4]));
        if used + w > room {
            break;
        }
        out.push(c);
        used += w;
    }
    if !fits && width > 0 {
        out.push('…');
        used += 1;
    }
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}

/// The lines of an expanded call: what it was given, a preview of what it
/// returned, and how it was paired with its result.
#[must_use]
pub fn call_detail(call: &Call) -> Vec<(Tone, String)> {
    let mut out = vec![(
        Tone::Normal,
        format!(
            "{} · {} · called {}",
            call.tool,
            call.role,
            call.called_at.with_timezone(&Local).format("%H:%M:%S")
        ),
    )];
    match call.paired {
        PairedBy::Id => {}
        PairedBy::Order => out.push((
            Tone::Warn,
            "paired by order: this log predates call ids, the result was matched to its call by \
             position"
                .to_string(),
        )),
        PairedBy::Unmatched => out.push((
            Tone::Warn,
            if call.returned_at.is_none() {
                "no result recorded: the call never returned (run interrupted)".to_string()
            } else {
                "no call recorded for this result".to_string()
            },
        )),
    }
    out.push((Tone::Normal, String::new()));
    out.push((Tone::Normal, "Arguments".to_string()));
    let args = serde_json::to_string_pretty(&call.input).unwrap_or_default();
    out.extend(args.lines().map(|l| (Tone::Dim, format!("  {l}"))));
    out.push((Tone::Normal, String::new()));
    let size = if call.output_chars > 0 {
        format!(" ({} chars)", call.output_chars)
    } else {
        String::new()
    };
    out.push((Tone::Normal, format!("Output preview{size}")));
    let tone = if call.is_error == Some(true) {
        Tone::Warn
    } else {
        Tone::Dim
    };
    out.extend(call.preview.lines().map(|l| (tone, format!("  {l}"))));
    out.push((Tone::Normal, String::new()));
    out.push((
        Tone::Dim,
        if call.output_file.is_some() {
            "o: complete output".to_string()
        } else {
            "the complete output was not traced".to_string()
        },
    ));
    out
}

/// Note shown above a complete output, or instead of it.
#[must_use]
pub fn output_note(call: &Call, found: bool) -> Option<String> {
    if !found {
        return Some(if call.output_file.is_some() {
            "The complete output is gone from .vibe/tool-output (vibe task discard or a manual \
             cleanup); only the preview is left."
                .to_string()
        } else {
            "The complete output was not traced (trace_outputs off, or a log from before 0.5); \
             only the preview is left."
                .to_string()
        });
    }
    (call.paired == PairedBy::Order)
        .then(|| "Paired by order: matched to its call by position, not by id.".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Duration as Span;
    use serde_json::json;
    use vibe_core::{AgentRole, CallId, Task, TaskStatus, TaskStore, Usage, VibeConfig};
    use vibe_pipeline::FileTaskStore;
    use vibe_pipeline::history::{HistoryFilter, project_history};
    use vibe_pipeline::store::EVENTS_FILE;

    pub(crate) fn at(second: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + Span::days(20_000) + Span::seconds(second)
    }

    fn env(second: i64, event: Event) -> Envelope {
        let mut e = Envelope::now(event);
        e.at = at(second);
        e
    }

    /// A tool call and its result, paired by id.
    pub(crate) fn call(
        run: RunId,
        second: i64,
        tool: &str,
        input: serde_json::Value,
    ) -> Vec<Envelope> {
        let call = CallId::new();
        vec![
            env(
                second,
                Event::ToolCalled {
                    run,
                    role: AgentRole::Coder,
                    tool: tool.into(),
                    input,
                    call,
                    subtask: None,
                },
            ),
            env(
                second + 1,
                Event::ToolReturned {
                    run,
                    role: AgentRole::Coder,
                    tool: tool.into(),
                    is_error: tool == "bash",
                    duration_ms: 1200,
                    preview: format!("{tool} output"),
                    call,
                    subtask: None,
                    exit_code: (tool == "bash").then_some(2),
                    timed_out: false,
                    output_chars: 12,
                    output_file: None,
                },
            ),
        ]
    }

    /// A task done in one run, with a commit and a tool call; a failed task.
    pub(crate) fn fixture_log(run: RunId, task: TaskId) -> Vec<Envelope> {
        let mut log = vec![env(0, Event::RunStarted { run, task })];
        log.extend(call(run, 1, "write_file", json!({"path": "src/export.rs"})));
        log.push(env(
            3,
            Event::Committed {
                run,
                subtask: None,
                commit: "0123456789abcdef".into(),
                message: "add export command".into(),
                files: vec!["src/export.rs".into()],
            },
        ));
        log.push(env(
            4,
            Event::ValidationFinished {
                run,
                command: "cargo test".into(),
                passed: true,
                exit_code: Some(0),
                integration: false,
            },
        ));
        log.push(env(
            600,
            Event::RunFinished {
                run,
                success: true,
                status: TaskStatus::Done,
                usage: Usage {
                    input_tokens: 1000,
                    output_tokens: 500,
                    ..Usage::default()
                },
                active_ms: 65_000,
                started_at: at(0),
            },
        ));
        log
    }

    /// The History of a temporary project with a done and a failed task.
    pub(crate) async fn fixture_history(root: &Path, all: bool) -> Vec<TaskHistory> {
        let store = FileTaskStore::open(root).unwrap();
        for (title, status) in [
            ("Add export command", TaskStatus::Done),
            ("Broken idea", TaskStatus::Failed),
        ] {
            let mut task = Task::new(title, "");
            task.status = status;
            task.updated_at = at(0);
            store.save_task(&task).await.unwrap();
            let log: String = fixture_log(RunId::new(), task.id)
                .iter()
                .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
                .collect();
            let dir = store.task_dir(task.id).await.unwrap();
            std::fs::write(dir.join(EVENTS_FILE), log).unwrap();
        }
        project_history(
            &store,
            root,
            &VibeConfig::default(),
            HistoryFilter { all },
            &|_: &Task| None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn history_rows_come_from_the_read_layer() {
        let dir = tempfile::tempdir().unwrap();
        let rows = fixture_history(dir.path(), false).await;
        assert_eq!(rows.len(), 1, "failed tasks are left out by default");
        let row = history_row(&rows[0], at(600) + Span::minutes(5));
        assert_eq!(
            row,
            [
                "001",
                "Add export command",
                "done",
                "1",
                "1",
                "1~",
                "1.5k",
                "1 min 05 s",
                "-",
                "5 min ago"
            ]
            .map(String::from)
        );
        let mut partial = rows[0].clone();
        partial.totals.complete = false;
        let row = history_row(&partial, at(600));
        assert_eq!((row[6].as_str(), row[7].as_str()), ("1.5k+", "1 min 05 s+"));
        let detail: Vec<String> = history_detail(&rows[0]).into_iter().map(|l| l.1).collect();
        let text = detail.join("\n");
        assert!(text.contains("#001 Add export command · done"), "{text}");
        assert!(
            text.contains("01234567 add export command (1 file(s))"),
            "{text}"
        );
        assert!(text.contains("from commit events, approximate"), "{text}");
        assert!(text.contains("? src/export.rs"), "{text}");
        assert!(text.contains("✓ cargo test"), "{text}");

        let all = fixture_history(tempfile::tempdir().unwrap().path(), true).await;
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn trace_data_defaults_to_the_last_run_and_switches_runs() {
        let (first, second) = (RunId::new(), RunId::new());
        let task = TaskId::new();
        let mut log = vec![env(0, Event::RunStarted { run: first, task })];
        log.extend(call(first, 1, "read_file", json!({"path": "a"})));
        log.push(env(10, Event::RunStarted { run: second, task }));
        log.extend(call(second, 11, "write_file", json!({"path": "b"})));
        log.extend(call(second, 13, "bash", json!({"command": "cargo test"})));
        let mut t = TraceView::default();
        t.set(TraceData::from_events(&log, Path::new("/p")));
        assert_eq!(t.current_run(), Some(second));
        assert_eq!(t.calls().len(), 2);
        t.step(5);
        assert_eq!(t.selected_call().unwrap().tool, "bash");
        let row = call_row(2, t.selected_call().unwrap(), "-");
        assert!(row.contains("bash"), "{row}");
        assert!(row.contains("1.2 s") && row.contains(" 2 error"), "{row}");

        t.expand();
        t.open_output();
        assert!(t.expanded && t.output.is_some());
        t.switch_run(-1);
        assert_eq!(t.current_run(), Some(first));
        assert!(!t.expanded && t.output.is_none(), "another run collapses");
        assert_eq!(t.selected_call().unwrap().tool, "read_file");
        t.switch_run(-1);
        assert_eq!(t.current_run(), Some(first), "stays on the first run");

        // Reloading keeps the run shown.
        t.set(TraceData::from_events(&log, Path::new("/p")));
        assert_eq!(t.current_run(), Some(first));
    }

    #[test]
    fn call_details_and_output_notes() {
        let run = RunId::new();
        let mut c = TraceData::from_events(
            &call(run, 0, "read_file", json!({"path": "src/lib.rs"})),
            Path::new("/p"),
        )
        .calls
        .remove(0);
        let text: Vec<String> = call_detail(&c).into_iter().map(|l| l.1).collect();
        let text = text.join("\n");
        assert!(text.contains("\"path\": \"src/lib.rs\""), "{text}");
        assert!(text.contains("read_file output"));
        assert!(text.contains("not traced"));
        assert!(output_note(&c, false).unwrap().contains("not traced"));
        assert_eq!(output_note(&c, true), None);
        c.paired = PairedBy::Order;
        assert!(output_note(&c, true).unwrap().contains("Paired by order"));
        assert!(
            call_detail(&c)
                .iter()
                .any(|l| l.1.starts_with("paired by order"))
        );
        c.output_file = Some("/p/.vibe/tool-output/x.txt".into());
        assert!(output_note(&c, false).unwrap().contains("gone"));
    }

    #[test]
    fn cells_are_fitted_to_the_displayed_width() {
        assert_eq!(fit("tool", 6), "tool  ");
        assert_eq!(fit("write_file", 6), "write…");
        // Two columns per CJK character.
        assert_eq!(fit("修正", 6), "修正  ");
        assert_eq!(fit("修正する", 6), "修正… ");
        assert_eq!(measure_text_width(&fit("🚀 launch", 6)), 6);
        assert_eq!(fit("x", 0), "");
    }

    #[test]
    fn every_logged_event_has_a_group() {
        let run = RunId::new();
        assert_eq!(
            Group::of(&Event::Merged {
                run,
                commit: "c".into(),
                branch: "b".into(),
                base: "main".into()
            }),
            Some(Group::Git)
        );
        assert_eq!(
            Group::of(&Event::Log {
                run: None,
                level: "info".into(),
                message: "m".into()
            }),
            Some(Group::Logs)
        );
        assert_eq!(
            Group::of(&Event::AgentDelta {
                run,
                role: AgentRole::Coder,
                subtask: None,
                delta: vibe_core::StreamDelta::Text { text: "x".into() },
            }),
            None
        );
    }
}
