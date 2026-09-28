//! Command line definition (clap derive types).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Top-level command line of the `vibe` binary.
#[derive(Debug, Parser)]
#[command(
    name = "vibe",
    version,
    about = "Autonomous multi-agent software development",
    long_about = "Describe a task; a pipeline of cooperating AI agents assesses it, writes a \
                  specification, plans the work, implements it in an isolated git worktree, \
                  reviews the result and hands you a branch ready to merge.",
    after_help = "Quick start:\n  vibe init\n  vibe task add \"Add a --json flag to the export command\"\n  \
                  vibe run 1\n  vibe task show 1\n\nTry it without an API key: vibe run 1 --provider mock --dry-run"
)]
pub struct Cli {
    /// Project directory (default: the current directory, or the nearest
    /// parent holding a `.vibe` directory).
    #[arg(short = 'C', long = "project", value_name = "DIR", global = true)]
    pub project: Option<PathBuf>,

    /// More logging (-v info, -vv debug, -vvv trace). `RUST_LOG` wins.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Machine-readable output (one JSON document, or one JSON event per line
    /// for `vibe run`).
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable colours (also disabled when `NO_COLOR` is set or the output
    /// is not a terminal).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Every `vibe` subcommand.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create `.vibe/config.toml` in the project.
    Init {
        /// Overwrite an existing configuration.
        #[arg(long)]
        force: bool,
    },
    /// Manage tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Run a task through the pipeline.
    Run(RunArgs),
    /// Approve what a paused run is waiting for (`pipeline.approvals`).
    Approve {
        /// Task number, `NNN-slug` directory name, or id prefix.
        #[arg(value_name = "REF")]
        reference: String,
        /// A note recorded with the approval.
        #[arg(long)]
        comment: Option<String>,
    },
    /// Reject what a paused run is waiting for; the reason goes back to the
    /// agents that produced it.
    Reject {
        /// Task number, `NNN-slug` directory name, or id prefix.
        #[arg(value_name = "REF")]
        reference: String,
        /// Why, in terms the agents can act on.
        #[arg(long)]
        reason: String,
    },
    /// Ask the process running a task to stop after its current step.
    Cancel {
        /// Task number, `NNN-slug` directory name, or id prefix.
        #[arg(value_name = "REF")]
        reference: String,
        /// Wait until the run has stopped.
        #[arg(long)]
        wait: bool,
    },
    /// Push a ready task's branch and open a pull request (or GitLab merge request).
    Pr(PrArgs),
    /// Show or clear the project memory (`.vibe/memory.jsonl`).
    #[command(subcommand)]
    Memory(MemoryCommand),
    /// Open the terminal UI: task board, live run view, approvals.
    Tui,
    /// Serve the HTTP API and the web UI on this machine.
    Serve(ServeArgs),
    /// Replay the logged events of a task's run, or of every task, optionally
    /// following new ones.
    Events(EventsArgs),
    /// Finished tasks: runs, commits, changed files, tokens and cost.
    History(HistoryArgs),
    /// The tool calls of a task's run: arguments, results and outputs.
    Trace(TraceArgs),
    /// Project summary: tasks per status, last runs, active worktrees.
    Status,
    /// Show or edit the configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Inspect and customise agents.
    #[command(subcommand)]
    Agents(AgentsCommand),
    /// Inspect plugins.
    #[command(subcommand)]
    Plugins(PluginsCommand),
    /// Check the environment and the configuration.
    Doctor,
    /// Print a shell completion script.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
}

/// `vibe task …`
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Create a task.
    Add {
        /// Short title.
        title: String,
        /// Description (`-` reads it from stdin).
        #[arg(short, long, conflicts_with = "description_file")]
        description: Option<String>,
        /// Read the description from a file (`-` for stdin).
        #[arg(long, value_name = "PATH")]
        description_file: Option<PathBuf>,
        /// Label (repeatable).
        #[arg(long = "label", value_name = "LABEL")]
        labels: Vec<String>,
    },
    /// List tasks (done and cancelled tasks are hidden unless `--all`).
    List {
        /// Only tasks with this status.
        #[arg(long, value_enum)]
        status: Option<StatusArg>,
        /// Include done and cancelled tasks.
        #[arg(long)]
        all: bool,
    },
    /// Create a task from a GitHub or GitLab issue.
    Import {
        /// `owner/repo#12`, `gitlab:group/project#5`, or an issue URL.
        #[arg(value_name = "ISSUE")]
        issue: String,
        /// Forge of the short form (`github` by default).
        #[arg(long, value_name = "FORGE", default_value = "github")]
        forge: String,
    },
    /// Show a task: spec, plan, QA verdict, run state and workspace.
    Show {
        /// Task number, `NNN-slug` directory name, or id prefix.
        #[arg(value_name = "REF")]
        reference: String,
    },
    /// Discard a task's workspace and delete the task.
    Discard {
        /// Task number, `NNN-slug` directory name, or id prefix.
        #[arg(value_name = "REF")]
        reference: String,
        /// Do not ask for confirmation (required when stdin is not a terminal).
        #[arg(short, long)]
        yes: bool,
    },
}

/// `vibe run …`
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Task number, `NNN-slug` directory name, or id prefix.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Force the complexity (skips the assessment).
    #[arg(long, value_enum)]
    pub complexity: Option<ComplexityArg>,
    /// Start at this phase, reusing persisted artefacts.
    #[arg(long, value_enum, value_name = "PHASE")]
    pub from: Option<PhaseArg>,
    /// Stop (resumable) once this phase is done.
    #[arg(long, value_enum, value_name = "PHASE")]
    pub until: Option<PhaseArg>,
    /// Only assess, specify and plan.
    #[arg(long)]
    pub dry_run: bool,
    /// Use this provider for every phase (its default model unless `--model`).
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Use this `provider/model` for every phase.
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
    /// Workspace provider: `git_worktree`, `in_place`, or one from a plugin.
    #[arg(long, value_name = "NAME")]
    pub workspace: Option<String>,
    /// Merge automatically after QA approval.
    #[arg(long)]
    pub auto_merge: bool,
    /// Drive the `mock` provider with scripted responses from a JSON file.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["provider", "model"])]
    pub script: Option<PathBuf>,
    /// Resume the last run of the task where it stopped.
    #[arg(long)]
    pub resume: bool,
    /// Pause the run once it used this many tokens (input plus output),
    /// counted across resumes. Overrides `pipeline.max_tokens`.
    #[arg(long, value_name = "TOKENS")]
    pub max_tokens: Option<u64>,
    /// Pause the run once it was active this long, counted across resumes:
    /// seconds, or a number followed by `s`, `m` or `h` (`90m`). Overrides
    /// `pipeline.max_duration_secs`.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_secs)]
    pub max_duration: Option<u64>,
}

/// Parse `90`, `90s`, `15m` or `2h` into seconds.
pub fn parse_duration_secs(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&text[..i], c.to_ascii_lowercase()),
        _ => (text, 's'),
    };
    let value: u64 = number
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration `{text}`: expected e.g. 90, 90s, 15m or 2h"))?;
    let factor = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(format!("invalid duration unit in `{text}`: use s, m or h")),
    };
    value
        .checked_mul(factor)
        .ok_or_else(|| format!("duration `{text}` is too large"))
}

/// `vibe pr …`
#[derive(Debug, Args)]
pub struct PrArgs {
    /// Task number, `NNN-slug` directory name, or id prefix.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Repository (`owner/repo`); default: read from the remote URL.
    #[arg(long, value_name = "OWNER/REPO")]
    pub repo: Option<String>,
    /// `github` or `gitlab`; default: from the remote host, else github.
    #[arg(long, value_name = "FORGE")]
    pub forge: Option<String>,
    /// Remote to push to.
    #[arg(long, default_value = "origin")]
    pub remote: String,
    /// Target branch; default: the branch the task was forked from.
    #[arg(long)]
    pub base: Option<String>,
    /// Open it as a draft.
    #[arg(long)]
    pub draft: bool,
    /// Do not push the branch (it is already on the remote).
    #[arg(long)]
    pub no_push: bool,
}

/// `vibe memory …`
#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// List the lessons, newest first, or the ones relevant to a query.
    List {
        /// Only entries sharing words with this text, most relevant first.
        #[arg(long)]
        query: Option<String>,
    },
    /// Forget everything.
    Clear {
        /// Do not ask for confirmation (required when stdin is not a terminal).
        #[arg(short, long)]
        yes: bool,
    },
}

/// `vibe serve …`
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on (loopback by default; anything else exposes the
    /// agents to the network).
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: String,
    /// Port (0 picks a free one).
    #[arg(long, default_value_t = 7777)]
    pub port: u16,
    /// Use this provider for every phase of the runs it starts.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Use this `provider/model` for every phase.
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
    /// Workspace provider.
    #[arg(long, value_name = "NAME")]
    pub workspace: Option<String>,
    /// Drive the `mock` provider with scripted responses from a JSON file.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["provider", "model"])]
    pub script: Option<std::path::PathBuf>,
    /// Directory of evaluation results (`evals/run_suite.py` destinations)
    /// shown in the web UI.
    #[arg(long, value_name = "DIR")]
    pub evals: Option<std::path::PathBuf>,
    /// Stop (as on `Ctrl-C`) when standard input is closed: for a parent
    /// process that starts the server with a pipe and may die without
    /// stopping it.
    #[arg(long)]
    pub exit_on_stdin_eof: bool,
}

/// `vibe events …`
#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Task number, `NNN-slug` directory name, or id prefix; without it,
    /// the events of every task.
    #[arg(value_name = "REF")]
    pub reference: Option<String>,
    /// Only events whose sequence number is greater than this.
    #[arg(long, value_name = "SEQ", default_value_t = 0, requires = "reference")]
    pub after: u64,
    /// Keep printing new events: until the run finishes or pauses with
    /// `REF`, until `Ctrl-C` without it.
    #[arg(short, long)]
    pub follow: bool,
    /// Every run of the task, not only the last one.
    #[arg(long, conflicts_with = "follow", requires = "reference")]
    pub all: bool,
    /// Only events after this time: RFC 3339 (`2026-09-27T10:00:00Z`) or an
    /// age (`30m`, `2h`, `1d`).
    #[arg(long, value_name = "TIME", value_parser = parse_since)]
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Only events of this type (repeatable), e.g. `run_finished`.
    #[arg(
        long = "type",
        value_name = "TYPE",
        value_parser = clap::builder::PossibleValuesParser::new(vibe_core::Event::TYPES),
        hide_possible_values = true
    )]
    pub types: Vec<String>,
    /// Only events of this task (repeatable): number, `NNN-slug` or id
    /// prefix.
    #[arg(long = "task", value_name = "REF")]
    pub tasks: Vec<String>,
}

/// Parse `--since`: an RFC 3339 time, or an age before now (`90s`, `30m`,
/// `2h`, `1d`).
pub fn parse_since(text: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    parse_since_at(text, chrono::Utc::now())
}

/// [`parse_since`] against an explicit "now" (for tests).
pub fn parse_since_at(
    text: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let text = text.trim();
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&chrono::Utc));
    }
    let invalid = || {
        format!(
            "invalid time `{text}`: expected RFC 3339 (2026-09-27T10:00:00Z) or an age (30m, 2h, 1d)"
        )
    };
    let Some((i, unit)) = text.char_indices().last() else {
        return Err(invalid());
    };
    let factor: i64 = match unit.to_ascii_lowercase() {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(invalid()),
    };
    let value: i64 = text[..i].trim().parse().map_err(|_| invalid())?;
    let secs = value
        .checked_mul(factor)
        .filter(|s| *s >= 0)
        .ok_or_else(invalid)?;
    chrono::Duration::try_seconds(secs)
        .and_then(|d| now.checked_sub_signed(d))
        .ok_or_else(|| format!("age `{text}` is too large"))
}

/// `vibe history …`
#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Task number, `NNN-slug` directory name, or id prefix; without it,
    /// every ready and done task.
    #[arg(value_name = "REF")]
    pub reference: Option<String>,
    /// Include failed and cancelled tasks.
    #[arg(long, conflicts_with = "reference")]
    pub all: bool,
}

/// `vibe trace …`
#[derive(Debug, Args)]
pub struct TraceArgs {
    /// Task number, `NNN-slug` directory name, or id prefix.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Run id or prefix (default: the last run).
    #[arg(long, value_name = "RUN", conflicts_with = "all")]
    pub run: Option<String>,
    /// Every run of the task.
    #[arg(long)]
    pub all: bool,
    /// Only calls of this tool.
    #[arg(long, value_name = "NAME")]
    pub tool: Option<String>,
    /// Only calls made for this subtask (id or prefix).
    #[arg(long, value_name = "ID")]
    pub subtask: Option<String>,
    /// Complete arguments and outputs (read from the trace store).
    #[arg(long)]
    pub full: bool,
}

/// `vibe config …`
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the effective configuration.
    Show {
        /// Print the built-in defaults instead.
        #[arg(long)]
        default: bool,
    },
    /// Print the path of the configuration file.
    Path,
    /// Set a value by dotted key, e.g. `pipeline.auto_merge true`.
    Set {
        /// Dotted key (`phases.plan.model`).
        key: String,
        /// Value: TOML literal (`true`, `3`, `["a"]`) or plain string.
        value: String,
    },
}

/// `vibe agents …`
#[derive(Debug, Subcommand)]
pub enum AgentsCommand {
    /// List every agent with its tools, thinking level, model and source.
    List,
    /// Print the resolved system prompt of an agent.
    Show {
        /// Role name (`planner`, `coder`, …).
        role: String,
    },
    /// Write an editable override file for an agent.
    Export {
        /// Role name (`planner`, `coder`, …).
        role: String,
        /// Target directory (default: `.vibe/agents`).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
}

/// `vibe plugins …`
#[derive(Debug, Subcommand)]
pub enum PluginsCommand {
    /// List declared and discovered plugins.
    List,
    /// Start every plugin, print what it offers, then stop it.
    Check,
}

/// Task status accepted on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum StatusArg {
    Backlog,
    Planning,
    Building,
    Review,
    Ready,
    Done,
    Failed,
    Cancelled,
}

impl From<StatusArg> for vibe_core::TaskStatus {
    fn from(s: StatusArg) -> Self {
        use vibe_core::TaskStatus as T;
        match s {
            StatusArg::Backlog => T::Backlog,
            StatusArg::Planning => T::Planning,
            StatusArg::Building => T::Building,
            StatusArg::Review => T::Review,
            StatusArg::Ready => T::Ready,
            StatusArg::Done => T::Done,
            StatusArg::Failed => T::Failed,
            StatusArg::Cancelled => T::Cancelled,
        }
    }
}

/// Complexity accepted on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[allow(missing_docs)]
pub enum ComplexityArg {
    Trivial,
    Simple,
    Standard,
    Complex,
}

impl From<ComplexityArg> for vibe_core::Complexity {
    fn from(c: ComplexityArg) -> Self {
        use vibe_core::Complexity as C;
        match c {
            ComplexityArg::Trivial => C::Trivial,
            ComplexityArg::Simple => C::Simple,
            ComplexityArg::Standard => C::Standard,
            ComplexityArg::Complex => C::Complex,
        }
    }
}

/// Pipeline phase accepted on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[allow(missing_docs)]
pub enum PhaseArg {
    Assess,
    Spec,
    Plan,
    Build,
    Qa,
    Fix,
    Merge,
}

impl From<PhaseArg> for vibe_core::Phase {
    fn from(p: PhaseArg) -> Self {
        use vibe_core::Phase as P;
        match p {
            PhaseArg::Assess => P::Assess,
            PhaseArg::Spec => P::Spec,
            PhaseArg::Plan => P::Plan,
            PhaseArg::Build => P::Build,
            PhaseArg::Qa => P::Qa,
            PhaseArg::Fix => P::Fix,
            PhaseArg::Merge => P::Merge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_run_flags() {
        let cli = Cli::try_parse_from([
            "vibe",
            "-C",
            "/tmp",
            "-vv",
            "run",
            "3",
            "--dry-run",
            "--provider",
            "mock",
            "--from",
            "plan",
            "--complexity",
            "simple",
        ])
        .unwrap();
        assert_eq!(cli.verbose, 2);
        let Command::Run(args) = cli.command else {
            panic!("expected run");
        };
        assert!(args.dry_run);
        assert_eq!(args.from, Some(PhaseArg::Plan));
        assert_eq!(args.complexity, Some(ComplexityArg::Simple));
    }

    #[test]
    fn budget_flags() {
        let Command::Run(args) = Cli::parse_from([
            "vibe",
            "run",
            "1",
            "--max-tokens",
            "50000",
            "--max-duration",
            "15m",
        ])
        .command
        else {
            panic!("not a run command");
        };
        assert_eq!(args.max_tokens, Some(50_000));
        assert_eq!(args.max_duration, Some(900));
        assert_eq!(parse_duration_secs("90"), Ok(90));
        assert_eq!(parse_duration_secs("2H"), Ok(7200));
        assert!(parse_duration_secs("3d").is_err());
        assert!(parse_duration_secs("m").is_err());
    }

    #[test]
    fn since_accepts_times_and_ages() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-27T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let at = |s: &str| parse_since_at(s, now).map(|t| t.to_rfc3339());
        assert_eq!(
            at("2026-09-27T10:00:00+02:00"),
            Ok("2026-09-27T08:00:00+00:00".into())
        );
        assert_eq!(at("30m"), Ok("2026-09-27T11:30:00+00:00".into()));
        assert_eq!(at("2H"), Ok("2026-09-27T10:00:00+00:00".into()));
        assert_eq!(at("1d"), Ok("2026-09-26T12:00:00+00:00".into()));
        assert_eq!(at("45s"), Ok("2026-09-27T11:59:15+00:00".into()));
        for bad in ["", "d", "30", "3w", "-2h", "yesterday", "2026-09-27"] {
            assert!(at(bad).is_err(), "{bad:?} accepted");
        }
        assert!(at("99999999999999d").is_err());
    }

    #[test]
    fn events_flags_and_type_validation() {
        let Command::Events(args) = Cli::try_parse_from([
            "vibe",
            "events",
            "--type",
            "run_finished",
            "--type",
            "committed",
            "--task",
            "2",
            "--since",
            "1h",
        ])
        .unwrap()
        .command
        else {
            panic!("not an events command");
        };
        assert_eq!(args.reference, None);
        assert_eq!(args.types, ["run_finished", "committed"]);
        assert_eq!(args.tasks, ["2"]);
        assert!(args.since.is_some());
        assert!(Cli::try_parse_from(["vibe", "events", "--type", "run_done"]).is_err());
        // `--after` and `--all` need a task.
        assert!(Cli::try_parse_from(["vibe", "events", "--after", "3"]).is_err());
        assert!(Cli::try_parse_from(["vibe", "events", "--all"]).is_err());
        assert!(Cli::try_parse_from(["vibe", "events", "1", "--all", "--type", "log"]).is_ok());
        assert!(Cli::try_parse_from(["vibe", "trace", "1", "--run", "ab", "--all"]).is_err());
        assert!(Cli::try_parse_from(["vibe", "history", "1", "--all"]).is_err());
    }
}
