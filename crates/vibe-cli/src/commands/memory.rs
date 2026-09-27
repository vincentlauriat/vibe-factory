//! `vibe memory`: the project memory shared by tasks.

use std::path::Path;

use anyhow::Result;
use serde_json::json;
use vibe_core::MemoryStore;
use vibe_pipeline::FileMemoryStore;

use crate::cli::MemoryCommand;
use crate::util::{Table, Ui, enum_name, relative_time, style, truncate};

/// Run a `vibe memory` subcommand.
pub async fn run(root: &Path, cmd: MemoryCommand, ui: Ui) -> Result<u8> {
    let store = FileMemoryStore::for_project(root);
    match cmd {
        MemoryCommand::List { query } => {
            let entries = match &query {
                Some(q) => store.recall(q, 50).await?,
                None => store.all().await?,
            };
            if ui.json {
                ui.print_json(&json!(entries));
                return Ok(0);
            }
            if entries.is_empty() {
                println!("The project memory is empty.");
                return Ok(0);
            }
            let mut table = Table::new(["kind", "recorded", "lesson"]);
            for e in &entries {
                table.row(vec![
                    enum_name(&e.kind),
                    relative_time(e.recorded_at),
                    truncate(&e.content, 100),
                ]);
            }
            print!("{}", table.render());
            Ok(0)
        }
        MemoryCommand::Clear { yes } => {
            if !yes && !crate::util::confirm("Forget every lesson of the project memory?", "--yes")?
            {
                println!("Aborted.");
                return Ok(1);
            }
            store.clear().await?;
            if ui.json {
                ui.print_json(&json!({"cleared": true}));
            } else {
                println!("{} project memory cleared", style::ok().apply_to("✓"));
            }
            Ok(0)
        }
    }
}
