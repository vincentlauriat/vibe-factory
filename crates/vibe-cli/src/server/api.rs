//! The HTTP API of `vibe serve`: the run manager's operations and the event
//! stream, as JSON over HTTP and server-sent events.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use vibe_core::{Envelope, Event, Task, TaskId, TaskStore};
use vibe_pipeline::{FileTaskStore, PipelineStore, RunManager, RunOptions};

use crate::app::AppContext;

/// How often a stream reads the task's event log again.
const STREAM_POLL: Duration = Duration::from_millis(300);

/// The single page of the web UI.
const INDEX_HTML: &str = include_str!("web/index.html");

/// Everything handlers need.
pub struct Inner {
    /// Configuration, store, workspace provider.
    pub ctx: Arc<AppContext>,
    /// Runs started through the server.
    pub manager: RunManager,
    /// Bearer token every API call must present.
    pub token: String,
    /// `Host` header values accepted (empty: any).
    pub allowed_hosts: Vec<String>,
    /// Directory searched for evaluation summaries, if any.
    pub evals: Option<std::path::PathBuf>,
}

/// Shared state of the server.
pub type ServerState = Arc<Inner>;

/// An API error: a status and a message, as `{"error": "…"}`.
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<vibe_core::Error> for ApiError {
    fn from(e: vibe_core::Error) -> Self {
        use vibe_core::ErrorKind;
        let status = match e.kind {
            ErrorKind::Config | ErrorKind::InvalidRequest | ErrorKind::Denied => {
                StatusCode::CONFLICT
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self(status, e.message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::CONFLICT, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// The router of the server.
pub fn router(state: ServerState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{task}", get(show_task))
        .route("/tasks/{task}/run", post(run_task))
        .route("/tasks/{task}/cancel", post(cancel_task))
        .route("/tasks/{task}/approve", post(approve))
        .route("/tasks/{task}/reject", post(reject))
        .route("/tasks/{task}/changes", get(changes))
        .route("/tasks/{task}/events", get(events))
        .route("/tasks/{task}/stream", get(stream))
        .route("/evals", get(evals))
        .layer(middleware::from_fn_with_state(state.clone(), authorize));
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .nest("/api", api)
        .layer(middleware::from_fn_with_state(state.clone(), check_host))
        .with_state(state)
}

/// Refuse requests whose `Host` is not the server's own name, so that a web
/// page on another site cannot reach the API through DNS rebinding.
async fn check_host(State(state): State<ServerState>, request: Request, next: Next) -> Response {
    if !state.allowed_hosts.is_empty() {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default();
        if !state.allowed_hosts.iter().any(|h| h == host) {
            return ApiError::new(StatusCode::FORBIDDEN, "unexpected Host header").into_response();
        }
    }
    next.run(request).await
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Every API call needs `Authorization: Bearer <token>`. Event streams may
/// pass `?token=` instead, since browsers cannot set headers on them.
async fn authorize(
    State(state): State<ServerState>,
    Query(query): Query<TokenQuery>,
    request: Request,
    next: Next,
) -> Response {
    let header_token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_string);
    let is_stream = request.uri().path().ends_with("/stream");
    let presented = header_token.or(if is_stream { query.token } else { None });
    if !presented.is_some_and(|t| constant_time_eq(t.as_bytes(), state.token.as_bytes())) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong token").into_response();
    }
    next.run(request).await
}

/// Compare without leaking where the first difference is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health() -> Json<Value> {
    Json(json!({"name": "vibe", "version": env!("CARGO_PKG_VERSION")}))
}

fn store(state: &ServerState) -> &Arc<FileTaskStore> {
    &state.ctx.store
}

async fn find(state: &ServerState, reference: &str) -> ApiResult<Task> {
    store(state)
        .find_by_prefix(reference)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                format!("no task matches `{reference}`"),
            )
        })
}

async fn row(state: &ServerState, task: &Task) -> ApiResult<Value> {
    let store = store(state);
    let number = store.entry(task.id).await?.map(|e| e.number);
    let running = state.manager.is_active(task.id) || store.is_running(task.id).await?;
    Ok(json!({"task": task, "number": number, "running": running}))
}

async fn list_tasks(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let mut rows = Vec::new();
    for task in store(&state).list_tasks().await? {
        rows.push(row(&state, &task).await?);
    }
    Ok(Json(Value::Array(rows)))
}

#[derive(Deserialize)]
struct NewTask {
    title: String,
    #[serde(default)]
    description: String,
}

async fn create_task(
    State(state): State<ServerState>,
    Json(body): Json<NewTask>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if body.title.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "the title is empty"));
    }
    let task = Task::new(body.title.trim(), body.description);
    store(&state).save_task(&task).await?;
    Ok((StatusCode::CREATED, Json(row(&state, &task).await?)))
}

async fn show_task(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<Json<Value>> {
    let task = find(&state, &reference).await?;
    let store = store(&state);
    let mut out = row(&state, &task).await?;
    out["run"] = json!(store.load_run_state(task.id).await?);
    out["spec"] = json!(store.load_spec(task.id).await?);
    out["plan"] = json!(store.load_plan(task.id).await?);
    out["qa_reports"] = json!(store.load_qa_reports(task.id).await?);
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
struct RunBody {
    #[serde(default)]
    resume: bool,
}

async fn run_task(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    body: Option<Json<RunBody>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let task = find(&state, &reference).await?;
    let resume = body.is_some_and(|b| b.resume);
    let handle = if resume {
        state.manager.resume(task.id, RunOptions::default())?
    } else {
        state.manager.start(task.id, RunOptions::default())?
    };
    // The run reports through events; its handle is not needed.
    tokio::spawn(async move {
        if let Err(e) = handle.wait().await {
            tracing::warn!(error = %e, "run failed");
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"task_id": task.id, "resumed": resume})),
    ))
}

async fn cancel_task(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let task = find(&state, &reference).await?;
    if !state.manager.cancel(task.id) {
        store(&state).request_cancel(task.id).await?;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"task_id": task.id}))))
}

#[derive(Deserialize, Default)]
struct Decision {
    #[serde(default)]
    comment: String,
    #[serde(default)]
    reason: String,
}

async fn approve(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    body: Option<Json<Decision>>,
) -> ApiResult<Json<Value>> {
    let task = find(&state, &reference).await?;
    let comment = body.map(|b| b.0.comment).unwrap_or_default();
    let record = crate::commands::approval::decide(store(&state), task.id, true, comment).await?;
    Ok(Json(json!(record)))
}

async fn reject(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Json(body): Json<Decision>,
) -> ApiResult<Json<Value>> {
    let task = find(&state, &reference).await?;
    let record =
        crate::commands::approval::decide(store(&state), task.id, false, body.reason).await?;
    Ok(Json(json!(record)))
}

async fn changes(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
) -> ApiResult<Json<Value>> {
    let task = find(&state, &reference).await?;
    let text = crate::tui::changes(&state.ctx, task.id).await?;
    Ok(Json(json!({"changes": text})))
}

/// Deepest directory level searched for `summary.json` files.
const EVALS_DEPTH: usize = 4;

/// Every `summary.json` written by `evals/run_suite.py` under the evals
/// directory, newest first: `[{"name", "modified", "summary"}]`.
async fn evals(State(state): State<ServerState>) -> ApiResult<Json<Value>> {
    let Some(dir) = state.evals.clone() else {
        return Ok(Json(json!({"enabled": false, "suites": []})));
    };
    let suites = tokio::task::spawn_blocking(move || {
        let mut found = Vec::new();
        let mut stack = vec![(dir.clone(), 0usize)];
        while let Some((current, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && depth < EVALS_DEPTH {
                    stack.push((path, depth + 1));
                } else if path.file_name().is_some_and(|n| n == "summary.json")
                    && let Ok(text) = std::fs::read_to_string(&path)
                    && let Ok(summary) = serde_json::from_str::<Value>(&text)
                {
                    let modified = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .map(chrono::DateTime::<chrono::Utc>::from);
                    let name = path
                        .parent()
                        .and_then(|p| p.strip_prefix(&dir).ok())
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    found.push(json!({"name": name, "modified": modified, "summary": summary}));
                }
            }
        }
        found.sort_by(|a, b| b["modified"].as_str().cmp(&a["modified"].as_str()));
        found
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({"enabled": true, "suites": suites})))
}

#[derive(Deserialize, Default)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    all: bool,
}

/// Logged events of the task's last run (or of every run with `all`).
async fn logged_events(
    state: &ServerState,
    task: TaskId,
    after: u64,
    all: bool,
) -> ApiResult<Vec<Envelope>> {
    let store = store(state);
    let run = if all {
        None
    } else {
        store.load_run_state(task).await?.map(|s| s.run_id)
    };
    Ok(store
        .load_events(task)
        .await?
        .into_iter()
        .filter(|e| all || (run.is_some() && e.event.run_id() == run))
        .filter(|e| after == 0 || e.seq.is_some_and(|s| s > after))
        .collect())
}

async fn events(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Json<Vec<Envelope>>> {
    let task = find(&state, &reference).await?;
    Ok(Json(
        logged_events(&state, task.id, query.after, query.all).await?,
    ))
}

/// Server-sent events of the task: its logged events after `after` (the
/// last run's), then new ones as they are logged, and the streamed text of
/// runs started by this server. A new run is sent from its beginning. Each
/// event's data is an envelope; its id is the sequence number.
async fn stream(
    State(state): State<ServerState>,
    Path(reference): Path<String>,
    Query(query): Query<EventsQuery>,
) -> ApiResult<Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>>> {
    let task = find(&state, &reference).await?;
    let (tx, rx) = mpsc::channel::<Envelope>(256);
    let mut live = state.manager.subscribe();
    let feeder = Arc::clone(&state);
    tokio::spawn(async move {
        let store = Arc::clone(store(&feeder));
        let mut run = store
            .load_run_state(task.id)
            .await
            .ok()
            .flatten()
            .map(|s| s.run_id);
        let mut last = query.after;
        loop {
            let current = store
                .load_run_state(task.id)
                .await
                .ok()
                .flatten()
                .map(|s| s.run_id);
            if current != run {
                run = current;
                last = 0;
            }
            if let Ok(log) = store.load_events(task.id).await {
                for envelope in log {
                    let seq = envelope.seq.unwrap_or(0);
                    if run.is_some() && envelope.event.run_id() == run && seq > last {
                        last = seq;
                        if tx.send(envelope).await.is_err() {
                            return;
                        }
                    }
                }
            }
            let deadline = tokio::time::sleep(STREAM_POLL);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    () = &mut deadline => break,
                    received = live.recv() => {
                        if let Ok(envelope) = received
                            && matches!(envelope.event, Event::AgentDelta { .. })
                            && envelope.event.run_id() == run
                            && tx.send(envelope).await.is_err()
                        {
                            return;
                        }
                    }
                }
            }
            if tx.is_closed() {
                return;
            }
        }
    });
    let events = futures::stream::unfold(rx, |mut rx| async move {
        let envelope = rx.recv().await?;
        let mut event = SseEvent::default()
            .event(envelope.event.type_name())
            .data(serde_json::to_string(&envelope).unwrap_or_default());
        if let Some(seq) = envelope.seq {
            event = event.id(seq.to_string());
        }
        Some((Ok(event), rx))
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
