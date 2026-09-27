//! `vibe events`: replay a run's event log, optionally following it live.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use vibe_core::{Event, EventSink, TaskStore};
use vibe_pipeline::PipelineStore;

use super::resolve_task;
use crate::app::open_store;
use crate::cli::EventsArgs;
use crate::render::Renderer;
use crate::util::Ui;

/// How often a followed log is read again.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);

/// Print the events of the task's last run (or of every run with `--all`)
/// whose sequence number is above `--after`; with `--follow`, keep reading
/// until the run finishes or pauses (both end with `run_finished`).
pub async fn run(root: &Path, args: EventsArgs, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    let task = resolve_task(&store, &args.reference).await?;
    let run = if args.all {
        None
    } else {
        match store.load_run_state(task.id).await? {
            Some(state) => Some(state.run_id),
            None => {
                if !ui.json {
                    println!("Task has not been run yet.");
                }
                return Ok(0);
            }
        }
    };
    let task_store: Arc<dyn TaskStore> = store.clone();
    let renderer = Renderer::new(ui.json, ui.verbose, task.id, task_store);
    let mut consumed = 0;
    loop {
        let events = store.load_events(task.id).await?;
        let mut ended = false;
        for envelope in events.iter().skip(consumed) {
            consumed += 1;
            let event_run = envelope.event.run_id();
            if run.is_some() && event_run != run {
                continue;
            }
            if envelope.seq.unwrap_or(0) <= args.after && args.after > 0 {
                continue;
            }
            renderer.on_event(envelope).await;
            // A paused run publishes `paused`, then `run_finished`.
            if matches!(envelope.event, Event::RunFinished { .. }) {
                ended = true;
            }
        }
        if !args.follow || ended {
            break;
        }
        tokio::time::sleep(FOLLOW_INTERVAL).await;
    }
    Ok(0)
}
