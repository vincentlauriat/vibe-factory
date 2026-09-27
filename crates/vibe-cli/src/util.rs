//! Small helpers shared by the commands: output settings, tables, time and
//! text formatting, project discovery.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use console::{Style, measure_text_width};
use vibe_core::config::VIBE_DIR;
use vibe_core::{Task, TaskStatus};

/// Output settings shared by every command.
#[derive(Debug, Clone, Copy)]
pub struct Ui {
    /// Print JSON instead of human-readable text.
    pub json: bool,
    /// Verbosity (`-v` count).
    pub verbose: u8,
}

impl Ui {
    /// Print a JSON value on one line. A closed standard output
    /// (`vibe --json … | head`) is not an error.
    pub fn print_json(&self, value: &serde_json::Value) {
        let _ = print_out(&format!("{value}\n"));
    }
}

/// Write `text` to standard output. `Ok(false)` when the reader went away
/// (`vibe … | head`): the command should then stop quietly.
pub fn print_out(text: &str) -> Result<bool> {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Decide whether colours are used and configure `console` accordingly.
pub fn setup_colors(no_color: bool) -> bool {
    let enabled = !no_color
        && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && std::io::stdout().is_terminal();
    console::set_colors_enabled(enabled);
    console::set_colors_enabled_stderr(enabled && std::io::stderr().is_terminal());
    enabled
}

/// Style helpers (no-ops when colours are disabled).
pub mod style {
    use super::Style;

    /// Bold text.
    pub fn bold() -> Style {
        Style::new().bold()
    }
    /// Dimmed text.
    pub fn dim() -> Style {
        Style::new().dim()
    }
    /// Success colour.
    pub fn ok() -> Style {
        Style::new().green()
    }
    /// Failure colour.
    pub fn err() -> Style {
        Style::new().red()
    }
    /// Warning colour.
    pub fn warn() -> Style {
        Style::new().yellow()
    }
    /// Accent colour for headers.
    pub fn accent() -> Style {
        Style::new().cyan().bold()
    }
}

/// Colour a task status.
pub fn styled_status(status: TaskStatus) -> String {
    let name = status_name(status);
    let s = match status {
        TaskStatus::Ready | TaskStatus::Done => style::ok(),
        TaskStatus::Failed => style::err(),
        TaskStatus::Review | TaskStatus::Planning | TaskStatus::Building => style::warn(),
        TaskStatus::Backlog | TaskStatus::Cancelled => style::dim(),
    };
    s.apply_to(name).to_string()
}

/// Stable lowercase name of a task status.
pub fn status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Backlog => "backlog",
        TaskStatus::Planning => "planning",
        TaskStatus::Building => "building",
        TaskStatus::Review => "review",
        TaskStatus::Ready => "ready",
        TaskStatus::Done => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

/// Every task status, in board order.
pub const ALL_STATUSES: [TaskStatus; 8] = [
    TaskStatus::Backlog,
    TaskStatus::Planning,
    TaskStatus::Building,
    TaskStatus::Review,
    TaskStatus::Ready,
    TaskStatus::Done,
    TaskStatus::Failed,
    TaskStatus::Cancelled,
];

/// Lowercase name of a serde enum value (`"standard"`, `"qa"`, …).
pub fn enum_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// A plain text table with aligned columns.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Table with these column headers.
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Append a row (cells may contain ANSI styles).
    pub fn row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    /// Whether the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Render with two spaces between columns; the last column is not padded.
    pub fn render(&self) -> String {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| measure_text_width(h)).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(measure_text_width(cell));
            }
        }
        let mut out = String::new();
        let line = |cells: &[String], out: &mut String, header: bool| {
            let mut text = String::new();
            for (i, cell) in cells.iter().enumerate().take(cols) {
                let cell = if header {
                    style::bold().apply_to(cell).to_string()
                } else {
                    cell.clone()
                };
                text.push_str(&cell);
                if i + 1 < cols {
                    let pad = widths[i].saturating_sub(measure_text_width(&cell)) + 2;
                    text.push_str(&" ".repeat(pad));
                }
            }
            out.push_str(text.trim_end());
            out.push('\n');
        };
        line(&self.headers, &mut out, true);
        for row in &self.rows {
            line(row, &mut out, false);
        }
        out
    }
}

/// "3 min ago", "2 h ago", "5 d ago", or a date.
pub fn relative_time(at: DateTime<Utc>) -> String {
    relative_time_from(at, Utc::now())
}

/// [`relative_time`] against an explicit "now" (for tests).
pub fn relative_time_from(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=2_592_000 => format!("{} d ago", secs / 86_400),
        _ => at.format("%Y-%m-%d").to_string(),
    }
}

/// Human duration: `850 ms`, `12.3 s`, `4 min 05 s`.
pub fn human_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", d.as_secs_f64())
    } else {
        let s = d.as_secs();
        format!("{} min {:02} s", s / 60, s % 60)
    }
}

/// Compact token count: `950`, `12.4k`, `1.2M`.
pub fn human_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// First 8 characters of a commit id, for display.
pub fn short_sha(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

/// Truncate to at most `max` characters, adding `…` when cut. Newlines are
/// replaced by spaces.
pub fn truncate(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        flat
    } else {
        let mut s: String = flat.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// The project root: `start` (default: the current directory) or its
/// nearest ancestor containing a `.vibe` directory. The search does not leave
/// the git repository `start` is in (it stops at the first directory that
/// contains `.git`) and never picks the home directory, where other tools
/// keep their own `.vibe`.
pub fn find_project_root(start: Option<&Path>) -> Result<PathBuf> {
    let start = match start {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    let start = std::path::absolute(&start)
        .with_context(|| format!("invalid project directory {}", start.display()))?;
    if !start.is_dir() {
        anyhow::bail!("project directory {} does not exist", start.display());
    }
    Ok(project_root_in(&start, dirs::home_dir().as_deref()))
}

fn project_root_in(start: &Path, home: Option<&Path>) -> PathBuf {
    for dir in start.ancestors() {
        if dir != start && home == Some(dir) {
            break;
        }
        if dir.join(VIBE_DIR).is_dir() {
            return dir.to_path_buf();
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    start.to_path_buf()
}

/// The project directory for `vibe init`: `start` or the current directory,
/// without searching parents.
pub fn init_root(start: Option<&Path>) -> Result<PathBuf> {
    let start = match start {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    let abs = std::path::absolute(&start)?;
    if !abs.is_dir() {
        anyhow::bail!("project directory {} does not exist", abs.display());
    }
    Ok(abs)
}

/// Ask a yes/no question on the terminal. Fails when stdin is not
/// interactive: destructive commands then need an explicit flag.
pub fn confirm(question: &str, flag: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "refusing to continue without confirmation: stdin is not a terminal (pass {flag})"
        );
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Short label of a task: `#3 Title`.
pub fn task_label(number: Option<u32>, task: &Task) -> String {
    match number {
        Some(n) => format!("#{n} {}", task.title),
        None => task.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_search_stops_at_the_repository_and_the_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let outer = home.join("outer");
        let repo = outer.join("repo");
        let deep = repo.join("src").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(home.join(VIBE_DIR)).unwrap();
        std::fs::create_dir_all(outer.join(VIBE_DIR)).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();

        // Inside a repository without `.vibe`: the start itself, not a parent.
        assert_eq!(project_root_in(&deep, Some(&home)), deep);
        // The repository root is the nearest `.vibe` once it exists.
        std::fs::create_dir(repo.join(VIBE_DIR)).unwrap();
        assert_eq!(project_root_in(&deep, Some(&home)), repo);
        // Outside any repository, parents are searched...
        let plain = outer.join("plain").join("sub");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(project_root_in(&plain, Some(&home)), outer);
        // ...but never up to the home directory's own `.vibe`.
        let loose = home.join("loose");
        std::fs::create_dir(&loose).unwrap();
        assert_eq!(project_root_in(&loose, Some(&home)), loose);
        // Running in the home directory itself still uses it.
        assert_eq!(project_root_in(&home, Some(&home)), home);
    }

    #[test]
    fn relative_times() {
        let now = Utc::now();
        assert_eq!(relative_time_from(now, now), "just now");
        assert_eq!(
            relative_time_from(now - chrono::Duration::minutes(5), now),
            "5 min ago"
        );
        assert_eq!(
            relative_time_from(now - chrono::Duration::hours(3), now),
            "3 h ago"
        );
        assert_eq!(
            relative_time_from(now - chrono::Duration::days(2), now),
            "2 d ago"
        );
    }

    #[test]
    fn truncation_and_units() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("a\nb", 5), "a b");
        assert_eq!(human_tokens(950), "950");
        assert_eq!(human_tokens(12_400), "12.4k");
        assert_eq!(
            human_duration(std::time::Duration::from_millis(850)),
            "850 ms"
        );
        assert_eq!(
            human_duration(std::time::Duration::from_secs(125)),
            "2 min 05 s"
        );
    }

    #[test]
    fn table_aligns_columns() {
        let mut t = Table::new(["#", "title"]);
        t.row(["1", "short"]);
        t.row(["10", "longer title"]);
        let out = t.render();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "1   short");
        assert_eq!(lines[2], "10  longer title");
    }
}
