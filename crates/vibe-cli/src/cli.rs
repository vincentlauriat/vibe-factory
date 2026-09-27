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
    /// Replay the logged events of a task's run, optionally following new ones.
    Events(EventsArgs),
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

/// `vibe events …`
#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Task number, `NNN-slug` directory name, or id prefix.
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Only events whose sequence number is greater than this.
    #[arg(long, value_name = "SEQ", default_value_t = 0)]
    pub after: u64,
    /// Keep printing new events until the run finishes or pauses.
    #[arg(short, long)]
    pub follow: bool,
    /// Every run of the task, not only the last one.
    #[arg(long, conflicts_with = "follow")]
    pub all: bool,
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
}
