//! `vibe events`: replay a run's event log, or the activity of every task,
//! optionally following it live.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::FutureExt;
use vibe_core::{Envelope, Event, TaskId, TaskStore};
use vibe_pipeline::{AllEventsFollower, EventCursor, FileTaskStore, PipelineStore, TaggedEnvelope};

use super::resolve_task;
use crate::app::open_store;
use crate::cli::EventsArgs;
use crate::render::Renderer;
use crate::util::{Ui, print_out, style};

/// How often a followed log is read again.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);

/// Filters shared by both modes.
struct Filter {
    since: Option<DateTime<Utc>>,
    types: Vec<String>,
}

impl Filter {
    fn keeps(&self, envelope: &Envelope) -> bool {
        self.since.is_none_or(|since| envelope.at > since)
            && (self.types.is_empty() || self.types.iter().any(|t| t == envelope.event.type_name()))
    }
}

/// `vibe events [REF]`: one task's run with `REF`, every task without.
pub async fn run(root: &Path, args: EventsArgs, ui: Ui) -> Result<u8> {
    let store = open_store(root)?;
    let filter = Filter {
        since: args.since,
        types: args.types.clone(),
    };
    let mut tasks = BTreeSet::new();
    for reference in &args.tasks {
        tasks.insert(resolve_task(&store, reference).await?.id);
    }
    match &args.reference {
        Some(reference) => {
            let task = resolve_task(&store, reference).await?;
            if !tasks.is_empty() && !tasks.contains(&task.id) {
                // `--task` and `REF` do not overlap: nothing to show.
                return Ok(0);
            }
            one_task(&store, task.id, &args, &filter, ui).await
        }
        None => every_task(&store, &tasks, args.follow, &filter, ui).await,
    }
}

/// Print the events of the task's last run (or of every run with `--all`)
/// whose sequence number is above `--after`; with `--follow`, keep reading
/// until the run finishes or pauses (both end with `run_finished`).
async fn one_task(
    store: &Arc<FileTaskStore>,
    task: TaskId,
    args: &EventsArgs,
    filter: &Filter,
    ui: Ui,
) -> Result<u8> {
    let run = if args.all {
        None
    } else {
        match store.load_run_state(task).await? {
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
    let renderer = Renderer::new(ui.json, ui.verbose, task, task_store);
    let mut consumed = 0;
    loop {
        let events = store.load_events(task).await?;
        let mut ended = false;
        for envelope in events.iter().skip(consumed) {
            consumed += 1;
            let event_run = envelope.event.run_id();
            if run.is_some() && event_run != run {
                continue;
            }
            // A paused run publishes `paused`, then `run_finished`; a resume
            // starts the same run again. The end counts whether or not the
            // filters show it.
            match envelope.event {
                Event::RunFinished { .. } => ended = true,
                Event::RunStarted { .. } => ended = false,
                _ => {}
            }
            if envelope.seq.unwrap_or(0) <= args.after && args.after > 0 {
                continue;
            }
            if !filter.keeps(envelope) {
                continue;
            }
            let text = if ui.json {
                format!("{}\n", serde_json::to_string(envelope)?)
            } else {
                match renderer.text(envelope).await {
                    Some(line) => format!("{line}\n"),
                    None => continue,
                }
            };
            if !print_out(&text)? {
                return Ok(0);
            }
        }
        if !args.follow || ended {
            break;
        }
        tokio::time::sleep(FOLLOW_INTERVAL).await;
    }
    Ok(0)
}

/// Print the events of every task (or of the `tasks` given) in time order,
/// each line tagged with its task number; with `follow`, keep printing new
/// ones (from now on unless `--since` says otherwise) until `Ctrl-C`.
async fn every_task(
    store: &Arc<FileTaskStore>,
    tasks: &BTreeSet<TaskId>,
    follow: bool,
    filter: &Filter,
    ui: Ui,
) -> Result<u8> {
    // Registered only when following, before the first read: `Ctrl-C`
    // during a long replay ends with 0 too. A plain listing keeps the
    // default `Ctrl-C` behaviour.
    let mut stop = follow.then(|| Box::pin(tokio::signal::ctrl_c()));
    let after = filter.since.map(EventCursor::since_time);
    let mut follower = if follow && after.is_none() {
        AllEventsFollower::from_end(store).await?
    } else {
        AllEventsFollower::new(store, after)
    };
    let mut feed = Feed {
        store: store.clone(),
        ui,
        renderers: HashMap::new(),
        reported: HashSet::new(),
    };
    loop {
        let polled = follower.poll().await?;
        for (task, error) in polled.errors {
            // A log that cannot be read stays unread: say so once.
            if feed.reported.insert(task) {
                eprintln!(
                    "{} cannot read the events of task {}: {error}",
                    style::warn().apply_to("warning:"),
                    task.short()
                );
            }
        }
        for tagged in &polled.events {
            if !tasks.is_empty() && !tasks.contains(&tagged.task) {
                continue;
            }
            if !filter.keeps(&tagged.envelope) {
                continue;
            }
            if !feed.print(tagged).await? {
                return Ok(0);
            }
            if let Some(stop) = stop.as_mut()
                && stop.as_mut().now_or_never().is_some()
            {
                return Ok(0);
            }
        }
        let Some(stop) = stop.as_mut() else {
            return Ok(0);
        };
        tokio::select! {
            _ = stop => return Ok(0),
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
        }
    }
}

/// Prints tagged events: one renderer per task (subtask labels come from
/// its plan), or the envelopes themselves with `--json`.
struct Feed {
    store: Arc<FileTaskStore>,
    ui: Ui,
    renderers: HashMap<TaskId, Renderer>,
    reported: HashSet<TaskId>,
}

impl Feed {
    /// Print one event; `Ok(false)` once standard output is closed.
    async fn print(&mut self, tagged: &TaggedEnvelope) -> Result<bool> {
        if self.ui.json {
            return print_out(&format!("{}\n", serde_json::to_string(tagged)?));
        }
        let renderer = self.renderers.entry(tagged.task).or_insert_with(|| {
            let task_store: Arc<dyn TaskStore> = self.store.clone();
            Renderer::new(false, self.ui.verbose, tagged.task, task_store)
        });
        let Some(text) = renderer.text(&tagged.envelope).await else {
            return Ok(true);
        };
        print_out(&format!("{}\n", tag_line(tagged.number, &text)))
    }
}

/// `#3 <line>`, keeping the blank line some events start with above the
/// tag.
fn tag_line(number: u32, text: &str) -> String {
    let body = text.trim_start_matches('\n');
    let blank = &text[..text.len() - body.len()];
    let tag = style::bold().apply_to(format!("#{number}"));
    format!("{blank}{tag} {body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_go_after_leading_blank_lines() {
        console::set_colors_enabled(false);
        assert_eq!(tag_line(3, "  ⟶ read_file()"), "#3   ⟶ read_file()");
        assert_eq!(tag_line(12, "\n── plan ──"), "\n#12 ── plan ──");
    }
}
