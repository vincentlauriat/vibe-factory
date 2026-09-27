//! `vibe approve` and `vibe reject`: answer the approval a paused run waits for.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;
use vibe_core::{Envelope, Event};
use vibe_pipeline::{PipelineStore, RunStatus};

use super::{resolve_task, task_number};
use crate::app::open_store;
use crate::util::{Ui, style};

/// Record the decision in `run.json` and in the run's event log. The run is
/// not resumed: `vibe run <REF> --resume` continues it.
pub async fn run(
    root: &Path,
    reference: &str,
    approved: bool,
    comment: String,
    ui: Ui,
) -> Result<u8> {
    let store = open_store(root)?;
    let task = resolve_task(&store, reference).await?;
    let mut state = store
        .load_run_state(task.id)
        .await?
        .context("the task has not been run yet")?;
    if state.status == RunStatus::Running {
        anyhow::bail!("the run is still running; wait until it pauses for approval");
    }
    let record = state.resolve_approval(approved, comment)?;
    store.save_run_state(&state).await?;

    let last_seq = store
        .load_events(task.id)
        .await?
        .iter()
        .filter(|e| e.event.run_id() == Some(state.run_id))
        .filter_map(|e| e.seq)
        .max()
        .unwrap_or(0);
    let mut envelope = Envelope::now(Event::ApprovalResolved {
        run: state.run_id,
        gate: record.gate,
        approved,
        comment: record.comment.clone(),
    });
    envelope.seq = Some(last_seq + 1);
    if let Some(sink) = store.event_sink(task.id, state.run_id).await? {
        sink.on_event(&envelope).await;
    }

    let reference = task_number(&store, &task)
        .await
        .map_or_else(|| task.id.short(), |n| n.to_string());
    if ui.json {
        ui.print_json(&json!({
            "task_id": task.id,
            "gate": record.gate,
            "approved": approved,
            "comment": record.comment,
        }));
    } else {
        let verb = if approved { "approved" } else { "rejected" };
        println!(
            "{} {} {verb}",
            style::ok().apply_to("✓"),
            style::bold().apply_to(format!("{} of task {reference}", record.gate))
        );
        println!("Continue with: vibe run {reference} --resume");
    }
    Ok(0)
}
