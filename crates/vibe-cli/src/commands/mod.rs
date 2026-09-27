//! Implementation of every subcommand.

pub mod agents;
pub mod approval;
pub mod config;
pub mod doctor;
pub mod events;
pub mod init;
pub mod memory;
pub mod plugins;
pub mod pr;
pub mod run;
pub mod status;
pub mod task;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use clap::CommandFactory;
use vibe_core::Task;
use vibe_pipeline::FileTaskStore;

use crate::cli::{Cli, Command};
use crate::util::{Ui, find_project_root, init_root};

/// Run the parsed command line; returns the process exit code.
pub async fn dispatch(cli: Cli) -> Result<u8> {
    let ui = Ui {
        json: cli.json,
        verbose: cli.verbose,
    };
    let project = cli.project.as_deref();
    match cli.command {
        Command::Init { force } => init::run(&init_root(project)?, force, ui).await,
        Command::Task(cmd) => task::run(&find_project_root(project)?, cmd, ui).await,
        Command::Run(args) => run::run(&find_project_root(project)?, args, ui).await,
        Command::Approve { reference, comment } => {
            approval::run(
                &find_project_root(project)?,
                &reference,
                true,
                comment.unwrap_or_default(),
                ui,
            )
            .await
        }
        Command::Reject { reference, reason } => {
            approval::run(&find_project_root(project)?, &reference, false, reason, ui).await
        }
        Command::Cancel { reference, wait } => {
            approval::cancel(&find_project_root(project)?, &reference, wait, ui).await
        }
        Command::Pr(args) => pr::run(&find_project_root(project)?, args, ui).await,
        Command::Memory(cmd) => memory::run(&find_project_root(project)?, cmd, ui).await,
        Command::Tui => crate::tui::run(&find_project_root(project)?).await,
        Command::Serve(args) => crate::server::run(&find_project_root(project)?, args, ui).await,
        Command::Events(args) => events::run(&find_project_root(project)?, args, ui).await,
        Command::Status => status::run(&find_project_root(project)?, ui).await,
        Command::Config(cmd) => config::run(&find_project_root(project)?, cmd, ui),
        Command::Agents(cmd) => agents::run(&find_project_root(project)?, cmd, ui),
        Command::Plugins(cmd) => plugins::run(&find_project_root(project)?, cmd, ui).await,
        Command::Doctor => doctor::run(&find_project_root(project)?, ui).await,
        Command::Completions { shell } => {
            let mut out = std::io::stdout();
            clap_complete::generate(shell, &mut Cli::command(), "vibe", &mut out);
            out.flush()?;
            Ok(0)
        }
    }
}

/// Find a task from a user reference (number, `NNN-slug`, id prefix).
pub async fn resolve_task(store: &Arc<FileTaskStore>, reference: &str) -> Result<Task> {
    store
        .find_by_prefix(reference)
        .await?
        .ok_or_else(|| anyhow!("no task matches `{reference}` (see `vibe task list`)"))
}

/// Sequence number of a task, if indexed.
pub async fn task_number(store: &FileTaskStore, task: &Task) -> Option<u32> {
    store.entry(task.id).await.ok().flatten().map(|e| e.number)
}

/// Whether the project has been initialised.
pub fn is_initialised(root: &Path) -> bool {
    crate::app::config_path(root).exists()
}
