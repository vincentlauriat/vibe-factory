//! Drawing the terminal UI from its [`App`] state.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Gauge, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};
use vibe_core::{SubtaskStatus, TaskStatus};

use super::app::{App, Budget, Screen, Tab, Tone};
use super::panes::{
    Group, HISTORY_COLUMNS, LoadState, call_detail, call_row, history_detail, history_row,
    output_note,
};
use crate::util::{human_duration, human_tokens, status_name};

/// Keys shown in the footer.
pub const HELP: &str =
    "↑↓ select  n new  r run  R resume  c cancel  a approve  x reject  tab view  q quit";

/// Keys of the Trace tab.
pub const TRACE_HELP: &str =
    "↑↓ call  enter expand  o output  [ ] run  g reload  esc back  tab view  q quit";

/// Keys of the Activity screen.
pub const FEED_HELP: &str = "↑↓ select  enter open task  f follow  1-7 filter  esc tasks  q quit";

/// Keys of the History screen.
pub const HISTORY_HELP: &str =
    "↑↓ select  enter details  a failed/cancelled  g reload  esc back  q quit";

/// Keys shown in the footer for what is on screen.
#[must_use]
pub fn help(app: &App) -> &'static str {
    match app.screen {
        Screen::Tasks if app.tab == Tab::Trace => TRACE_HELP,
        Screen::Tasks => HELP,
        Screen::Activity => FEED_HELP,
        Screen::History => HISTORY_HELP,
    }
}

fn tone_style(tone: Tone) -> Style {
    match tone {
        Tone::Normal => Style::default(),
        Tone::Dim => Style::default().fg(Color::DarkGray),
        Tone::Good => Style::default().fg(Color::Green),
        Tone::Warn => Style::default().fg(Color::Yellow),
        Tone::Bad => Style::default().fg(Color::Red),
    }
}

fn status_style(status: TaskStatus) -> Style {
    match status {
        TaskStatus::Ready | TaskStatus::Done => Style::default().fg(Color::Green),
        TaskStatus::Failed => Style::default().fg(Color::Red),
        TaskStatus::Review => Style::default().fg(Color::Yellow),
        TaskStatus::Planning | TaskStatus::Building => Style::default().fg(Color::Cyan),
        TaskStatus::Backlog | TaskStatus::Cancelled => Style::default().fg(Color::DarkGray),
    }
}

/// Draw the whole UI.
pub fn draw(frame: &mut Frame, app: &App) {
    let [title, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let running = app.tasks.iter().filter(|t| t.running).count();
    let mut spans = vec![
        Span::styled(" vibe ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "· {} · {} task(s), {running} running   ",
            app.project,
            app.tasks.len()
        )),
    ];
    for screen in Screen::ALL {
        let style = if screen == app.screen {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            tone_style(Tone::Dim)
        };
        spans.push(Span::styled(format!(" {} ", screen.title()), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), title);
    match app.screen {
        Screen::Tasks => {
            let [board, detail] =
                Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
                    .areas(body);
            draw_board(frame, app, board);
            draw_detail(frame, app, detail);
        }
        Screen::Activity => draw_feed(frame, app, body),
        Screen::History => draw_history(frame, app, body),
    }
    draw_footer(frame, app, footer);
}

/// Text standing for data being loaded or that failed to load, if any.
fn load_note(load: &LoadState, empty: bool) -> Option<Line<'static>> {
    match load {
        LoadState::Failed(e) => Some(Line::styled(
            format!("cannot load: {e}"),
            tone_style(Tone::Bad),
        )),
        LoadState::Wanted | LoadState::Loading(_) if empty => {
            Some(Line::styled("Loading…", tone_style(Tone::Dim)))
        }
        _ => None,
    }
}

fn loading_mark(load: &LoadState) -> &'static str {
    if matches!(load, LoadState::Loading(_)) {
        " · loading…"
    } else {
        ""
    }
}

fn draw_feed(frame: &mut Frame, app: &App, area: Rect) {
    let feed = &app.feed;
    let mut filters = vec![Span::raw(" Activity · ")];
    for (i, group) in Group::ALL.iter().enumerate() {
        let style = if feed.shows(*group) {
            Style::default()
        } else {
            tone_style(Tone::Dim).add_modifier(Modifier::CROSSED_OUT)
        };
        filters.push(Span::styled(format!("{} {}", i + 1, group.name()), style));
        filters.push(Span::raw(" "));
    }
    filters.push(Span::raw(format!(
        "· follow {}{} ",
        if feed.follow { "on" } else { "off" },
        loading_mark(&feed.load)
    )));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(filters));
    let visible = feed.visible();
    if let Some(note) = load_note(&feed.load, visible.is_empty()) {
        frame.render_widget(Paragraph::new(note).block(block), area);
        return;
    }
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No event to show.", tone_style(Tone::Dim))).block(block),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = visible
        .iter()
        .map(|l| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", l.label()), Style::default().fg(Color::Cyan)),
                Span::styled(
                    l.at.with_timezone(&chrono::Local)
                        .format("%H:%M:%S ")
                        .to_string(),
                    tone_style(Tone::Dim),
                ),
                Span::styled(l.text.clone(), tone_style(l.tone)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(feed.selection());
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let h = &app.history;
    let title = format!(
        " History · {}{} ",
        if h.all {
            "every finished task"
        } else {
            "ready and done (a: also failed and cancelled)"
        },
        loading_mark(&h.load)
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    if let Some(note) = load_note(&h.load, h.rows.is_empty()) {
        frame.render_widget(Paragraph::new(note).block(block), area);
        return;
    }
    if h.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No finished task yet.", tone_style(Tone::Dim)))
                .block(block),
            area,
        );
        return;
    }
    if h.open
        && let Some(row) = h.selected_row()
    {
        let lines: Vec<Line> = history_detail(row)
            .into_iter()
            .map(|(tone, text)| Line::styled(text, tone_style(tone)))
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((h.scroll, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let now = chrono::Utc::now();
    let rows: Vec<Row> = h
        .rows
        .iter()
        .map(|r| {
            Row::new(history_row(r, now).into_iter().enumerate().map(|(i, c)| {
                if i == 2 {
                    Cell::from(c).style(status_style(r.task.status))
                } else {
                    Cell::from(c)
                }
            }))
        })
        .collect();
    let widths = [
        Constraint::Length(4),
        Constraint::Min(16),
        Constraint::Length(9),
        Constraint::Length(4),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(12),
    ];
    let header = Row::new(HISTORY_COLUMNS).style(Style::default().add_modifier(Modifier::BOLD));
    let mut state = TableState::default();
    state.select(Some(h.selected));
    frame.render_stateful_widget(
        Table::new(rows, widths)
            .header(header)
            .block(block)
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

fn draw_board(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|t| {
            let marker = if t.running { "● " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::raw(format!("{} ", t.label())),
                Span::raw(t.title.clone()),
                Span::raw(" "),
                Span::styled(status_name(t.status), status_style(t.status)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Tasks "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !app.tasks.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(list, area, &mut state);
    if app.tasks.is_empty() {
        let inner = area.inner(ratatui::layout::Margin::new(2, 1));
        frame.render_widget(
            Paragraph::new("No task yet: press n to add one.").style(tone_style(Tone::Dim)),
            inner,
        );
    }
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(task) = app.selected_task() else {
        frame.render_widget(Block::default().borders(Borders::ALL), area);
        return;
    };
    let [header, gauges, tabs, content] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(area);

    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} {}", task.label(), task.title),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(status_name(task.status), status_style(task.status)),
    ])];
    let run_line = match &app.detail.state {
        Some(s) => {
            let mut text = format!("run {}: {}", enum_name_of(&s.status), s.current_phase);
            if let Some(gate) = s.pending_approval {
                text.push_str(&format!(" · waiting for approval of the {gate}"));
            } else if let Some(err) = &s.last_error {
                text.push_str(&format!(" · {}", crate::util::truncate(err, 80)));
            }
            Line::styled(
                text,
                if s.pending_approval.is_some() {
                    tone_style(Tone::Warn)
                } else {
                    tone_style(Tone::Dim)
                },
            )
        }
        None => Line::styled("not run yet (r to run)", tone_style(Tone::Dim)),
    };
    lines.push(run_line);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::TOP)),
        header,
    );
    draw_budget(frame, app.detail.budget, gauges);

    let titles: Vec<&str> = Tab::ALL.iter().map(|t| t.title()).collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        tabs,
    );
    let block = Block::default().borders(Borders::ALL);
    match app.tab {
        Tab::Activity => draw_activity(frame, app, content, block),
        Tab::Plan => draw_plan(frame, app, content, block),
        Tab::Changes => {
            let text = app
                .detail
                .changes
                .clone()
                .unwrap_or_else(|| "Loading…".to_string());
            let text = if text.trim().is_empty() {
                "No change in the workspace.".to_string()
            } else {
                text
            };
            frame.render_widget(Paragraph::new(text).block(block), content);
        }
        Tab::Trace => draw_trace(frame, app, content, block),
    }
}

fn draw_trace(frame: &mut Frame, app: &App, area: Rect, block: Block) {
    let t = &app.detail.trace;
    if let Some(note) = load_note(&t.load, t.data.runs.is_empty()) {
        frame.render_widget(Paragraph::new(note).block(block), area);
        return;
    }
    let Some(run) = t.current_run() else {
        frame.render_widget(Paragraph::new("No run yet.").block(block), area);
        return;
    };
    let calls = t.calls();
    let block = block.title(format!(
        " run {}/{} · {} · {} call(s){} ",
        t.run + 1,
        t.data.runs.len(),
        run.short(),
        calls.len(),
        loading_mark(&t.load)
    ));
    if let (Some(output), Some(call)) = (&t.output, t.selected_call()) {
        let mut lines = vec![Line::styled(
            format!("Complete output of {} (esc: back)", call.tool),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        match &output.load {
            LoadState::Ready => {
                if let Some(note) = output_note(call, output.text.is_some()) {
                    lines.push(Line::styled(note, tone_style(Tone::Warn)));
                }
                // Only the lines that can be seen, borrowed from the lines
                // split once when the output was loaded.
                let height = usize::from(area.height);
                lines.extend(
                    output
                        .lines
                        .iter()
                        .skip(output.scroll)
                        .take(height)
                        .map(|l| Line::raw(l.as_str())),
                );
            }
            LoadState::Failed(e) => lines.push(Line::styled(
                format!("cannot read the output: {e}"),
                tone_style(Tone::Bad),
            )),
            LoadState::Wanted | LoadState::Loading(_) => {
                lines.push(Line::styled("Loading…", tone_style(Tone::Dim)));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    if t.expanded
        && let Some(call) = t.selected_call()
    {
        let lines: Vec<Line> = call_detail(call)
            .into_iter()
            .map(|(tone, text)| Line::styled(text, tone_style(tone)))
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((t.scroll, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    if calls.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "No tool call in this run.",
                tone_style(Tone::Dim),
            ))
            .block(block),
            area,
        );
        return;
    }
    let [header, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(block.inner(area));
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "{:>4} {:<9} {:<16} {:<14} {:>9} {:>4} err",
                "seq", "role", "subtask", "tool", "duration", "exit"
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        header,
    );
    let items: Vec<ListItem> = calls
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let subtask = c
                .subtask
                .map_or_else(|| "-".to_string(), |s| app.subtask_title(s));
            let tone = if c.is_error == Some(true) || c.timed_out {
                Tone::Warn
            } else {
                Tone::Normal
            };
            ListItem::new(Line::styled(call_row(i + 1, c, &subtask), tone_style(tone)))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(t.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        list,
        &mut state,
    );
}

fn enum_name_of<T: serde::Serialize>(value: &T) -> String {
    crate::util::enum_name(value)
}

fn draw_budget(frame: &mut Frame, budget: Option<Budget>, area: Rect) {
    let [tokens, time] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let Some(b) = budget else {
        frame.render_widget(
            Paragraph::new("budget: nothing used yet").style(tone_style(Tone::Dim)),
            tokens,
        );
        return;
    };
    let ratio = |used: u64, limit: Option<u64>| {
        limit
            .filter(|l| *l > 0)
            .map(|l| (used as f64 / l as f64).clamp(0.0, 1.0))
    };
    let tokens_label = match b.token_limit {
        Some(l) => format!("tokens {} / {}", human_tokens(b.tokens), human_tokens(l)),
        None => format!("tokens {}", human_tokens(b.tokens)),
    };
    let time_label = match b.duration_limit_ms {
        Some(l) => format!(
            "active {} / {}",
            human_duration(Duration::from_millis(b.active_ms)),
            human_duration(Duration::from_millis(l))
        ),
        None => format!(
            "active {}",
            human_duration(Duration::from_millis(b.active_ms))
        ),
    };
    for (area, label, r) in [
        (tokens, tokens_label, ratio(b.tokens, b.token_limit)),
        (time, time_label, ratio(b.active_ms, b.duration_limit_ms)),
    ] {
        match r {
            Some(r) => frame.render_widget(
                Gauge::default()
                    .gauge_style(Style::default().fg(if r >= 0.9 {
                        Color::Red
                    } else {
                        Color::Cyan
                    }))
                    .ratio(r)
                    .label(label),
                area,
            ),
            None => frame.render_widget(Paragraph::new(label), area),
        }
    }
}

fn draw_activity(frame: &mut Frame, app: &App, area: Rect, block: Block) {
    let height = usize::from(area.height.saturating_sub(2));
    let streaming = app.detail.streaming.trim();
    let reserved = if streaming.is_empty() {
        0
    } else {
        3.min(height)
    };
    let mut lines: Vec<Line> = app
        .detail
        .activity
        .iter()
        .rev()
        .take(height.saturating_sub(reserved))
        .map(|a| Line::styled(a.text.clone(), tone_style(a.tone)))
        .collect();
    lines.reverse();
    if !streaming.is_empty() {
        let tail: String = {
            let chars: Vec<char> = streaming.chars().collect();
            let keep = usize::from(area.width.saturating_sub(4)) * reserved;
            chars[chars.len().saturating_sub(keep)..].iter().collect()
        };
        lines.push(Line::styled(
            format!("  … {tail}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    if lines.is_empty() {
        lines.push(Line::styled("No activity yet.", tone_style(Tone::Dim)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_plan(frame: &mut Frame, app: &App, area: Rect, block: Block) {
    let Some(plan) = &app.detail.plan else {
        frame.render_widget(Paragraph::new("No plan yet.").block(block), area);
        return;
    };
    let items: Vec<ListItem> = plan
        .subtasks()
        .enumerate()
        .map(|(i, s)| {
            let (icon, tone) = match s.status {
                SubtaskStatus::Done => ("✓", Tone::Good),
                SubtaskStatus::InProgress => ("●", Tone::Normal),
                SubtaskStatus::Failed => ("✗", Tone::Bad),
                SubtaskStatus::Skipped => ("–", Tone::Dim),
                SubtaskStatus::Pending => ("○", Tone::Dim),
            };
            ListItem::new(Line::styled(
                format!("{icon} {}. {}", i + 1, s.title),
                tone_style(tone),
            ))
        })
        .collect();
    frame.render_widget(List::new(items).block(block), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(input) = &app.input {
        Line::from(vec![
            Span::styled(
                format!("{}: ", input.prompt()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(input.buffer.clone()),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ])
    } else if let Some((tone, text)) = &app.message {
        Line::styled(text.clone(), tone_style(*tone))
    } else {
        Line::styled(help(app), tone_style(Tone::Dim))
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{Activity, TaskRow};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use vibe_core::{ApprovalGate, Phase, RunId, Task};
    use vibe_pipeline::RunState;

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn empty_board_invites_to_add_a_task() {
        let app = App {
            project: "demo".into(),
            ..App::default()
        };
        let screen = render(&app, 100, 20);
        assert!(screen.contains("vibe"));
        assert!(screen.contains("press n to add one"));
        assert!(screen.contains("q quit"));
    }

    #[test]
    fn board_detail_budget_and_approval_are_drawn() {
        let mut task = Task::new("Add export command", "");
        task.status = TaskStatus::Review;
        let mut app = App {
            project: "demo".into(),
            ..App::default()
        };
        let mut row = TaskRow::new(&task, Some(3), true);
        row.running = true;
        app.set_tasks(vec![
            row,
            TaskRow::new(&Task::new("Other", ""), Some(2), false),
        ]);
        app.select_detail(task.id);
        let mut state = RunState::new(RunId::new(), task.id, Phase::Build);
        state.pending_approval = Some(ApprovalGate::Plan);
        app.detail.state = Some(state);
        app.detail.budget = Some(Budget {
            tokens: 1500,
            token_limit: Some(3000),
            active_ms: 65_000,
            duration_limit_ms: None,
        });
        app.detail.activity.push_back(Activity {
            seq: Some(1),
            tone: Tone::Normal,
            text: "● plan".into(),
        });
        app.detail.streaming = "writing the plan".into();
        let screen = render(&app, 120, 24);
        assert!(screen.contains("003 Add export command"), "{screen}");
        assert!(screen.contains("1 running"));
        assert!(screen.contains("waiting for approval of the plan"));
        assert!(screen.contains("tokens 1.5k / 3.0k"), "{screen}");
        assert!(screen.contains("active 1 min 05 s"));
        assert!(screen.contains("● plan"));
        assert!(screen.contains("writing the plan"));

        app.tab = Tab::Plan;
        assert!(render(&app, 120, 24).contains("No plan yet."));
        app.input = Some(crate::tui::app::Input {
            kind: crate::tui::app::InputKind::Reject(task.id),
            buffer: "too broad".into(),
        });
        assert!(render(&app, 120, 24).contains("Reason of the rejection: too broad"));
    }

    #[tokio::test]
    async fn activity_history_and_trace_screens_are_drawn() {
        use crate::tui::panes::tests::{call, fixture_history};
        use crate::tui::panes::{LoadState, OutputView, TraceData};
        use vibe_core::{Envelope, Event};
        use vibe_pipeline::TaggedEnvelope;

        let task = Task::new("Add export command", "");
        let mut app = App {
            project: "demo".into(),
            ..App::default()
        };
        app.set_tasks(vec![TaskRow::new(&task, Some(1), false)]);
        let run = RunId::new();

        // Activity.
        app.screen = Screen::Activity;
        app.feed.load = LoadState::Ready;
        app.push_feed(&[TaggedEnvelope {
            task: task.id,
            number: 1,
            envelope: Envelope::now(Event::PhaseStarted {
                run,
                phase: Phase::Build,
            }),
        }]);
        app.feed.toggle(2);
        let screen = render(&app, 140, 20);
        assert!(screen.contains("A Activity"), "{screen}");
        assert!(screen.contains("1 agents"), "{screen}");
        assert!(screen.contains("follow on"), "{screen}");
        assert!(screen.contains("#001"), "{screen}");
        assert!(screen.contains("● build"), "{screen}");
        assert!(screen.contains("1-7 filter"), "{screen}");

        // History, then the detail of its row.
        app.screen = Screen::History;
        assert!(render(&app, 140, 20).contains("Loading…"));
        let dir = tempfile::tempdir().unwrap();
        app.history.rows = fixture_history(dir.path(), false).await;
        app.history.load = LoadState::Ready;
        let screen = render(&app, 140, 20);
        for text in [
            "commits",
            "finished",
            "Add export command",
            "1.5k",
            "1 min 05 s",
            "done",
        ] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        assert!(screen.contains("a failed/cancelled"), "{screen}");
        app.history.open = true;
        let screen = render(&app, 140, 30);
        assert!(screen.contains("Commits (1)"), "{screen}");
        assert!(screen.contains("Changed files (1)"), "{screen}");

        // Trace: the list, an expanded call, its complete output.
        app.screen = Screen::Tasks;
        app.select_detail(task.id);
        app.tab = Tab::Trace;
        let mut log = vec![Envelope::now(Event::RunStarted { run, task: task.id })];
        log.extend(call(
            run,
            1,
            "read_file",
            serde_json::json!({"path": "src/lib.rs"}),
        ));
        app.detail
            .trace
            .set(TraceData::from_events(&log, std::path::Path::new("/p")));
        let screen = render(&app, 140, 24);
        assert!(screen.contains("run 1/1"), "{screen}");
        assert!(screen.contains("seq role"), "{screen}");
        assert!(screen.contains("read_file"), "{screen}");
        assert!(screen.contains("o output"), "{screen}");
        app.detail.trace.expand();
        let screen = render(&app, 140, 30);
        assert!(screen.contains("Arguments"), "{screen}");
        assert!(screen.contains("src/lib.rs"), "{screen}");
        let mut output = OutputView::default();
        output.set(None, "read_file output");
        app.detail.trace.output = Some(output);
        let screen = render(&app, 140, 30);
        assert!(screen.contains("Complete output of read_file"), "{screen}");
        assert!(screen.contains("was not traced"), "{screen}");
        assert!(
            screen.contains("read_file output"),
            "the preview stands in: {screen}"
        );
    }

    #[test]
    fn every_screen_draws_at_tiny_sizes_without_panicking() {
        use crate::tui::panes::{LoadState, OutputView, TraceData, tests::call};
        use vibe_core::{Envelope, Event};

        let task = Task::new("A task with a rather long title 修正", "");
        let mut app = App {
            project: "demo".into(),
            ..App::default()
        };
        app.set_tasks(vec![TaskRow::new(&task, Some(1), true)]);
        app.select_detail(task.id);
        let run = RunId::new();
        app.detail.state = Some(RunState::new(run, task.id, Phase::Build));
        app.detail.streaming = "streamed text".into();
        let mut log = vec![Envelope::now(Event::RunStarted { run, task: task.id })];
        log.extend(call(run, 1, "bash", serde_json::json!({"command": "ls"})));
        app.detail
            .trace
            .set(TraceData::from_events(&log, std::path::Path::new("/p")));
        app.push_feed(&[vibe_pipeline::TaggedEnvelope {
            task: task.id,
            number: 1,
            envelope: log[0].clone(),
        }]);
        app.feed.load = LoadState::Ready;
        app.history.load = LoadState::Failed("boom".into());

        let mut states: Vec<App> = Vec::new();
        for tab in Tab::ALL {
            let mut a = app.clone();
            a.tab = tab;
            states.push(a);
        }
        let mut expanded = states[3].clone();
        expanded.detail.trace.expand();
        states.push(expanded.clone());
        let mut output = OutputView::default();
        output.set(Some("line\n".repeat(50)), "");
        expanded.detail.trace.output = Some(output);
        states.push(expanded);
        for screen in [Screen::Activity, Screen::History] {
            let mut a = app.clone();
            a.screen = screen;
            states.push(a.clone());
            a.history.load = LoadState::Ready;
            a.history.open = true;
            states.push(a);
        }
        for state in &states {
            for width in 0..=40 {
                for height in 0..=8 {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|f| draw(f, state)).unwrap();
                }
            }
        }
    }
}
