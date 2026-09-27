//! # vibe
//!
//! Command line interface of the **Vibe Factory** framework.
//!
//! ```text
//! vibe init                          write .vibe/config.toml
//! vibe task add "<title>"            create a task
//! vibe task list | show | discard    inspect or delete tasks
//! vibe run <ref>                     run the pipeline, rendering events live
//! vibe status                        tasks per status, last runs, worktrees
//! vibe config show | path | set      configuration
//! vibe agents list | show | export   agents and their prompts
//! vibe plugins list | check          plugins
//! vibe doctor                        environment checks
//! vibe completions <shell>           shell completions
//! ```
//!
//! ## Wiring of a run
//!
//! `vibe run` loads `.vibe/config.toml`, applies the command line overrides
//! (`--provider`, `--model`, `--workspace`, `--auto-merge`, `--script`),
//! registers the built-in agents and the `.vibe/agents` overrides, the
//! built-in tools, the configured providers (plus the CLI's mock provider
//! when `mock` is used or `--script` is given), starts the plugins and
//! registers their contributions, picks the workspace provider (assisted
//! merges resolve their model from the `merge` phase), opens the file task
//! store and runs the pipeline with a git committer when the project is a
//! git repository.
//!
//! ## Exit codes
//!
//! `0` success (task ready or done, or a run stopped as requested by
//! `--dry-run` / `--until`), `1` failure or error, `2` task waiting for a
//! human (review, paused), `130` cancelled.

#![forbid(unsafe_code)]

mod app;
mod cli;
mod commands;
mod mock;
mod render;
mod util;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(verbose >= 2)
        .without_time()
        .try_init();
}

fn print_error(err: &anyhow::Error) {
    let label = util::style::err().apply_to("error:");
    eprintln!("{label} {err}");
    for cause in err.chain().skip(1) {
        eprintln!("  caused by: {cause}");
    }
}

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    util::setup_colors(cli.no_color);
    init_tracing(cli.verbose);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            print_error(&anyhow::Error::new(e).context("cannot start the async runtime"));
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(commands::dispatch(cli)) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            print_error(&err);
            ExitCode::FAILURE
        }
    }
}
