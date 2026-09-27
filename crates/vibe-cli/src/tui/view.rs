//! Drawing the terminal UI from its [`App`] state.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use vibe_core::{SubtaskStatus, TaskStatus};

use super::app::{App, Budget, Tab, Tone};
use crate::util::{human_duration, human_tokens, status_name};

/// Keys shown in the footer.
pub const HELP: &str =
    "↑↓ select  n new  r run  R resume  c cancel  a approve  x reject  tab view  q quit";

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
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" vibe ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "· {} · {} task(s), {running} running",
                app.project,
                app.tasks.len()
            )),
        ])),
        title,
    );
    let [board, detail] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);
    draw_board(frame, app, board);
    draw_detail(frame, app, detail);
    draw_footer(frame, app, footer);
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
    }
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
        Line::styled(HELP, tone_style(Tone::Dim))
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
}
