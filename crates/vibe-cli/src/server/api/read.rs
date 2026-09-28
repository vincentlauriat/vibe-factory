//! Read-only routes over the whole project: every task's events, the global
//! event stream, task histories and tool-call traces.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use vibe_core::{CallId, Event, RunId, Task, TaskId};
use vibe_pipeline::{
    AllEventsFollower, EventCursor, FileTaskStore, HistoryFilter, PipelineStore, RunManager,
    RunTrace, TaggedEnvelope, TaskHistory, pair_all_calls, project_history, read_output,
    task_history, trace,
};

use super::{ApiError, ApiResult, STREAM_POLL, ServerState, find, store};
use crate::app::load_config;
use crate::commands::history::task_branch;
use crate::commands::trace::{PrefixMatch, find_prefix, runs_of};

/// Events returned by `GET /api/events` without `limit`.
const EVENTS_LIMIT: usize = 1000;

/// Largest `limit` of `GET /api/events`.
const EVENTS_MAX: usize = 10_000;

/// Interval of the comment lines that keep an idle stream open.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// Global streams open at once; more are refused with 503.
const MAX_STREAMS: usize = 32;

/// Events the shared reader keeps for streams that fall behind.
const HUB_CAPACITY: usize = 1024;

/// Largest output served by `GET /api/tasks/{task}/trace/{call}/output`;
/// longer ones are cut and marked.
const OUTPUT_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Response header of `GET /api/events`: the cursor of the last event read,
/// before the filters, to resume from with `after`.
pub const CURSOR_HEADER: &str = "x-vibe-cursor";

/// Response header of `GET /api/events`: how many task logs could not be
/// read (their events are missing from the answer).
pub const READ_ERRORS_HEADER: &str = "x-vibe-read-errors";

/// Response header of a call output cut at [`OUTPUT_MAX_BYTES`].
pub const TRUNCATED_HEADER: &str = "x-vibe-truncated";

/// An event for the streams: its cursor (none for streamed text) and the
/// tagged envelope.
type Item = Arc<(Option<EventCursor>, TaggedEnvelope)>;

/// One reader of every task's log shared by all global streams: it polls
/// the logs and listens to the run manager while at least one stream is
/// open, and publishes what it finds to them.
pub struct StreamHub {
    sender: broadcast::Sender<Item>,
    /// Whether the reader runs; taken with subscriptions so that it never
    /// stops while a stream joins.
    running: Mutex<bool>,
    slots: Arc<Semaphore>,
}

impl Default for StreamHub {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamHub {
    /// A hub whose reader starts with the first stream.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sender: broadcast::channel(HUB_CAPACITY).0,
            running: Mutex::new(false),
            slots: Arc::new(Semaphore::new(MAX_STREAMS)),
        }
    }

    fn subscribe(
        self: &Arc<Self>,
        store: &Arc<FileTaskStore>,
        manager: &RunManager,
    ) -> broadcast::Receiver<Item> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let receiver = self.sender.subscribe();
        if !*running {
            *running = true;
            tokio::spawn(run_hub(
                Arc::clone(self),
                Arc::clone(store),
                manager.clone(),
            ));
        }
        receiver
    }
}

/// The shared reader: new logged events of every task, each once per task
/// in cursor order (a log truncated or replaced is read again from its
/// start; what was already published is not published again), and the
/// streamed text of the runs of the manager, tagged with their task. It
/// stops when no stream listens any more.
async fn run_hub(hub: Arc<StreamHub>, store: Arc<FileTaskStore>, manager: RunManager) {
    let mut follower = match AllEventsFollower::from_end(&store).await {
        Ok(follower) => follower,
        Err(error) => {
            tracing::warn!(%error, "cannot read the task index");
            AllEventsFollower::new(&store, Some(EventCursor::since_time(chrono::Utc::now())))
        }
    };
    let mut live = Some(manager.subscribe());
    // Task of each run seen, to tag the streamed text.
    let mut runs: HashMap<RunId, (TaskId, u32)> = HashMap::new();
    let mut published: HashMap<TaskId, EventCursor> = HashMap::new();
    let mut reported: HashSet<TaskId> = HashSet::new();
    loop {
        {
            let mut running = hub.running.lock().unwrap_or_else(|e| e.into_inner());
            if hub.sender.receiver_count() == 0 {
                *running = false;
                return;
            }
        }
        match follower.poll().await {
            Ok(polled) => {
                for (task, error) in polled.errors {
                    if reported.insert(task) {
                        tracing::warn!(%task, %error, "cannot read the events of a task");
                    }
                }
                for e in polled.events {
                    let cursor = e.cursor();
                    if published.get(&e.task).is_some_and(|last| cursor <= *last) {
                        continue;
                    }
                    published.insert(e.task, cursor);
                    if let Some(run) = e.envelope.event.run_id() {
                        runs.insert(run, (e.task, e.number));
                    }
                    let _ = hub.sender.send(Arc::new((Some(cursor), e)));
                }
            }
            Err(error) => tracing::warn!(%error, "cannot read the task index"),
        }
        let mut looked_up = false;
        let deadline = tokio::time::sleep(STREAM_POLL);
        tokio::pin!(deadline);
        while let Some(receiver) = live.as_mut() {
            let received = tokio::select! {
                () = &mut deadline => break,
                received = receiver.recv() => received,
            };
            let envelope = match received {
                Ok(envelope) => envelope,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    live = None;
                    break;
                }
            };
            let Event::AgentDelta { run, .. } = envelope.event else {
                continue;
            };
            // A run already going when the reader started: its
            // `run_started` came before.
            if !runs.contains_key(&run) && !looked_up {
                looked_up = true;
                learn_active_runs(&store, &manager, &mut runs).await;
            }
            if let Some(&(task, number)) = runs.get(&run) {
                let tagged = TaggedEnvelope {
                    task,
                    number,
                    envelope,
                };
                let _ = hub.sender.send(Arc::new((None, tagged)));
            }
        }
        if live.is_none() {
            deadline.await;
        }
    }
}

/// Record the run of every task the run manager is running.
async fn learn_active_runs(
    store: &FileTaskStore,
    manager: &RunManager,
    runs: &mut HashMap<RunId, (TaskId, u32)>,
) {
    for task in manager.active() {
        let run = store.load_run_state(task).await.ok().flatten();
        let number = store.entry(task).await.ok().flatten().map(|e| e.number);
        if let (Some(run), Some(number)) = (run, number) {
            runs.insert(run.run_id, (task, number));
        }
    }
}

/// Filters and starting point shared by `GET /api/events` and
/// `GET /api/stream`, from a query string where `type` and `task` repeat.
#[derive(Default)]
struct Filters {
    types: HashSet<String>,
    tasks: HashSet<TaskId>,
    after: Option<String>,
    since: Option<String>,
    limit: Option<usize>,
}

impl Filters {
    async fn parse(state: &ServerState, pairs: Vec<(String, String)>) -> ApiResult<Self> {
        let mut out = Self::default();
        for (key, value) in pairs {
            match key.as_str() {
                "type" => {
                    if !Event::TYPES.contains(&value.as_str()) {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            format!("unknown event type `{value}`"),
                        ));
                    }
                    out.types.insert(value);
                }
                "task" => {
                    out.tasks.insert(find(state, &value).await?.id);
                }
                "after" => out.after = Some(value),
                "since" => out.since = Some(value),
                "limit" => {
                    out.limit = Some(value.parse().map_err(|_| {
                        ApiError::new(StatusCode::BAD_REQUEST, format!("invalid limit `{value}`"))
                    })?);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Where to start: after the `Last-Event-ID` of a reconnecting client,
    /// else after `after`, else after `since` (a time or an age, as
    /// `vibe events --since`); `None` when none is given.
    fn start(&self, last_event_id: Option<&str>) -> ApiResult<Option<EventCursor>> {
        if let Some(id) = last_event_id.or(self.after.as_deref()) {
            return id.parse().map(Some).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid event cursor `{id}`"),
                )
            });
        }
        match &self.since {
            Some(since) => crate::cli::parse_since(since)
                .map(|t| Some(EventCursor::since_time(t)))
                .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e)),
            None => Ok(None),
        }
    }

    fn keeps(&self, e: &TaggedEnvelope) -> bool {
        (self.types.is_empty() || self.types.contains(e.envelope.event.type_name()))
            && (self.tasks.is_empty() || self.tasks.contains(&e.task))
    }
}

/// `GET /api/events`: logged events of every task, in cursor order, the
/// newest last. Logs that cannot be read are counted in
/// [`READ_ERRORS_HEADER`] and logged.
pub(super) async fn all_events(
    State(state): State<ServerState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> ApiResult<Response> {
    let filters = Filters::parse(&state, pairs).await?;
    let after = filters.start(None)?;
    let polled = AllEventsFollower::new(store(&state), after).poll().await?;
    for (task, error) in &polled.errors {
        tracing::warn!(%task, %error, "cannot read the events of a task");
    }
    let last = polled.events.last().map(TaggedEnvelope::cursor);
    let mut kept: Vec<TaggedEnvelope> = polled
        .events
        .into_iter()
        .filter(|e| filters.keeps(e))
        .collect();
    let limit = filters.limit.unwrap_or(EVENTS_LIMIT).min(EVENTS_MAX);
    if kept.len() > limit {
        kept.drain(..kept.len() - limit);
    }
    let mut response = Json(kept).into_response();
    let headers = response.headers_mut();
    headers.insert(READ_ERRORS_HEADER, HeaderValue::from(polled.errors.len()));
    if let Some(cursor) = last
        && let Ok(value) = HeaderValue::from_str(&cursor.to_string())
    {
        headers.insert(CURSOR_HEADER, value);
    }
    Ok(response)
}

/// What a stream sent last per task, to drop what it receives twice (the
/// replay and the shared reader overlap).
#[derive(Default)]
struct Sent {
    per_task: HashMap<TaskId, EventCursor>,
    /// Largest cursor sent (or the starting point).
    last: Option<EventCursor>,
}

impl Sent {
    /// Whether the event at `cursor` of `task` is new to this stream,
    /// recording it if so.
    fn fresh(&mut self, cursor: EventCursor, task: TaskId) -> bool {
        if self.per_task.get(&task).is_some_and(|last| cursor <= *last) {
            return false;
        }
        self.per_task.insert(task, cursor);
        self.last = self.last.max(Some(cursor));
        true
    }
}

/// Send the logged events after `after` that the stream has not sent yet.
/// `Err` when the client is gone.
async fn replay(
    store: &FileTaskStore,
    after: EventCursor,
    filters: &Filters,
    sent: &mut Sent,
    tx: &mpsc::Sender<(Option<EventCursor>, TaggedEnvelope)>,
) -> Result<(), ()> {
    let polled = match AllEventsFollower::new(store, Some(after)).poll().await {
        Ok(polled) => polled,
        Err(error) => {
            tracing::warn!(%error, "cannot read the task index");
            return Ok(());
        }
    };
    for (task, error) in &polled.errors {
        tracing::warn!(%task, %error, "cannot read the events of a task");
    }
    for e in polled.events {
        let cursor = e.cursor();
        if sent.fresh(cursor, e.task) && filters.keeps(&e) {
            tx.send((Some(cursor), e)).await.map_err(|_| ())?;
        }
    }
    Ok(())
}

/// `GET /api/stream`: server-sent events of every task. Logged events after
/// the starting point (see [`Filters::start`]; from now on without one),
/// then new ones as they are logged, tasks created later included, and the
/// streamed text (`agent_delta`) of runs started by this server. Each SSE
/// event is named by its type and its data is a [`TaggedEnvelope`]; logged
/// events carry their [`EventCursor`] as id, streamed text has no id.
///
/// Every stream shares one reader of the logs ([`StreamHub`]); at most
/// [`MAX_STREAMS`] are open at once (503 beyond).
pub(super) async fn stream(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>>> {
    let filters = Filters::parse(&state, pairs).await?;
    let last_event_id = headers.get("last-event-id").and_then(|h| h.to_str().ok());
    let start = filters.start(last_event_id)?;
    let permit = Arc::clone(&state.streams.slots)
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "too many event streams are open",
            )
        })?;
    // Subscribe before the replay: what is logged meanwhile comes from both,
    // and `Sent` drops the second copy.
    let mut shared = state.streams.subscribe(store(&state), &state.manager);
    let store = Arc::clone(store(&state));
    let (tx, rx) = mpsc::channel::<(Option<EventCursor>, TaggedEnvelope)>(256);
    tokio::spawn(async move {
        let _permit = permit;
        let mut sent = Sent {
            last: start,
            ..Sent::default()
        };
        if let Some(after) = start
            && replay(&store, after, &filters, &mut sent, &tx)
                .await
                .is_err()
        {
            return;
        }
        loop {
            let received = tokio::select! {
                () = tx.closed() => return,
                received = shared.recv() => received,
            };
            let item = match received {
                Ok(item) => item,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Read what was missed from the logs.
                    let after = sent
                        .last
                        .unwrap_or_else(|| EventCursor::since_time(chrono::Utc::now()));
                    if replay(&store, after, &filters, &mut sent, &tx)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let (cursor, tagged) = &*item;
            if let Some(cursor) = cursor
                && (start.is_some_and(|s| *cursor <= s) || !sent.fresh(*cursor, tagged.task))
            {
                continue;
            }
            if filters.keeps(tagged) && tx.send((*cursor, tagged.clone())).await.is_err() {
                return;
            }
        }
    });
    let events = futures::stream::unfold(rx, |mut rx| async move {
        let (cursor, tagged) = rx.recv().await?;
        let mut event = SseEvent::default()
            .event(tagged.envelope.event.type_name())
            .data(serde_json::to_string(&tagged).unwrap_or_default());
        if let Some(cursor) = cursor {
            event = event.id(cursor.to_string());
        }
        Some((Ok(event), rx))
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(HEARTBEAT)))
}

#[derive(Deserialize, Default)]
pub(super) struct HistoryQuery {
    #[serde(default)]
    all: bool,
}

/// `GET /api/history`: finished tasks (also failed and cancelled ones with
/// `all`), most recent activity first. The configuration is the project's,
/// as for `vibe history`, not the server's overrides.
pub(super) async fn history(
    State(state): State<ServerState>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<TaskHistory>>> {
    let root = &state.ctx.root;
    let config = load_config(root)?;
    let branch_of = |task: &Task| task_branch(root, &config, task);
    let filter = HistoryFilter { all: query.all };
    Ok(Json(
        project_history(&**store(&state), root, &config, filter, &branch_of).await?,
    ))
}

/// `GET /api/history/{task}`: the history of any task.
pub(super) async fn task_history_of(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<Json<TaskHistory>> {
    let task = find(&state, &reference).await?;
    let root = &state.ctx.root;
    let config = load_config(root)?;
    let branch = task_branch(root, &config, &task);
    Ok(Json(
        task_history(&**store(&state), root, &config, task, branch.as_deref()).await?,
    ))
}

#[derive(Deserialize, Default)]
pub(super) struct TraceQuery {
    run: Option<String>,
    #[serde(default)]
    all: bool,
}

/// `GET /api/tasks/{task}/trace`: the tool calls of the last run, of the
/// run `run` (an id or a unique prefix, as `vibe trace --run`), or of every
/// run with `all`, in log order. 404 when the task has not been run.
pub(super) async fn trace_of(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Query(query): Query<TraceQuery>,
) -> ApiResult<Response> {
    let task = find(&state, &reference).await?;
    let root = &state.ctx.root;
    let events = store(&state).load_events(task.id).await?;
    let runs = runs_of(&events);
    if runs.is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "the task has not been run yet",
        ));
    }
    let run_trace = |run: RunId| {
        let calls = trace::pair_calls(&events, run, root);
        RunTrace {
            run,
            files_written: trace::files_written(&calls),
            calls,
        }
    };
    if query.all {
        if query.run.is_some() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "`run` and `all` exclude each other",
            ));
        }
        let traces: Vec<RunTrace> = runs.into_iter().map(run_trace).collect();
        return Ok(Json(traces).into_response());
    }
    let run = match &query.run {
        Some(prefix) => match find_prefix(prefix, &runs) {
            PrefixMatch::One(run) => run,
            PrefixMatch::None => {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    format!("no run of this task matches `{}`", prefix.trim()),
                ));
            }
            PrefixMatch::Several => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("`{}` matches several runs of this task", prefix.trim()),
                ));
            }
        },
        None => trace::last_run(&events).unwrap_or(runs[runs.len() - 1]),
    };
    Ok(Json(run_trace(run)).into_response())
}

/// `GET /api/tasks/{task}/trace/{call}/output`: the complete output of a
/// call, as text. Only files inside `.vibe/tool-output/` are read (the read
/// layer drops other recorded paths and refuses links that lead out); a
/// refused or unreadable file answers like a missing one, the reason going
/// to the server's log only. Outputs longer than [`OUTPUT_MAX_BYTES`] are
/// cut, marked at the end and in [`TRUNCATED_HEADER`].
pub(super) async fn call_output(
    State(state): State<ServerState>,
    Path((reference, call)): Path<(String, String)>,
) -> ApiResult<Response> {
    let id = CallId::parse(&call)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid call id `{call}` (12 hex digits expected)"),
            )
        })?;
    let task = find(&state, &reference).await?;
    let root = &state.ctx.root;
    let events = store(&state).load_events(task.id).await?;
    let call = pair_all_calls(&events, root)
        .into_iter()
        .find(|c| c.call == id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("no call `{call}` in this task"),
            )
        })?;
    let text = match read_output(root, &call).await {
        Ok(Some(text)) => text,
        Ok(None) => return Err(not_traced()),
        Err(error) => {
            tracing::warn!(call = %id, %error, "cannot serve a call output");
            return Err(not_traced());
        }
    };
    let (text, truncated) = cut(text, OUTPUT_MAX_BYTES);
    let mut response =
        ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response();
    if truncated {
        response
            .headers_mut()
            .insert(TRUNCATED_HEADER, HeaderValue::from_static("true"));
    }
    Ok(response)
}

fn not_traced() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "the output of this call was not traced, or was discarded",
    )
}

/// `text` cut to at most `max` bytes on a character boundary, with a note
/// of what was left out; whether it was cut.
fn cut(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let rest = text.len() - end;
    text.truncate(end);
    text.push_str(&format!("\n… (output cut: {rest} more bytes)\n"));
    (text, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_cut_on_a_character_boundary() {
        assert_eq!(cut("short".into(), 10), ("short".into(), false));
        // "aéaéaéaé" is 12 bytes; 4 bytes end after the second `a`, 5 inside
        // the second `é`.
        let (text, was_cut) = cut("aé".repeat(4), 4);
        assert!(was_cut);
        assert_eq!(text, "aéa\n… (output cut: 8 more bytes)\n");
        let (text, _) = cut("aé".repeat(4), 5);
        assert!(text.starts_with("aéa\n"), "{text}");
    }
}
