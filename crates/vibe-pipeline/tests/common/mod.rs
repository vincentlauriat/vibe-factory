//! Shared helpers for the pipeline integration tests.

#![allow(dead_code, missing_docs)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use vibe_agents::test_support::text_response;
use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, Envelope, Error, Event, EventBus, EventSink,
    HookDecision, InPlaceWorkspace, ModelProvider, ModelRef, Phase, Registry, Result, Task,
    TaskStore, ToolRegistry, VibeConfig,
};
use vibe_pipeline::{
    Committer, FileTaskStore, Pipeline, PipelineDeps, PipelineStore, ProviderResolver, Resetter,
};

/// Marker found in the system prompt of each built-in role.
pub const ASSESSOR: &str = "You are the complexity assessor";
pub const GATHERER: &str = "You are the requirements gatherer";
pub const RESEARCHER: &str = "You are the technical researcher";
pub const WRITER: &str = "You are the specification writer";
pub const CRITIC: &str = "You are the specification critic";
pub const PLANNER: &str = "You are the planner";
pub const CODER: &str = "You are a coding agent";
pub const RECOVERY: &str = "You are the recovery agent";
pub const REVIEWER: &str = "You are the QA reviewer";
pub const FIXER: &str = "You are the QA fixer";

pub const ROLES: &[(&str, &str)] = &[
    (ASSESSOR, "assessor"),
    (GATHERER, "gatherer"),
    (RESEARCHER, "researcher"),
    (WRITER, "writer"),
    (CRITIC, "critic"),
    (PLANNER, "planner"),
    (CODER, "coder"),
    (RECOVERY, "recovery"),
    (REVIEWER, "reviewer"),
    (FIXER, "fixer"),
];

struct Route {
    marker: String,
    key: Option<String>,
    delay: Duration,
    responses: VecDeque<CompletionResponse>,
}

/// One request received by the router.
#[derive(Debug, Clone)]
pub struct Call {
    /// Role name (see [`ROLES`]).
    pub role: String,
    /// Route key that matched, if any.
    pub key: Option<String>,
    /// Full system prompt.
    pub system: String,
    /// First user message.
    pub user: String,
}

/// Provider answering by role (system prompt marker) and optional key
/// (substring of the system prompt), safe under concurrency.
#[derive(Default)]
pub struct RouterProvider {
    routes: Mutex<Vec<Route>>,
    calls: Mutex<Vec<Call>>,
    completed: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

fn role_of(system: &str) -> String {
    ROLES
        .iter()
        .find(|(m, _)| system.contains(m))
        .map_or("unknown", |(_, n)| n)
        .to_string()
}

impl RouterProvider {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Queue `responses` for `marker` (and `key`, when given).
    pub fn route(&self, marker: &str, key: Option<&str>, responses: Vec<CompletionResponse>) {
        self.route_delayed(marker, key, Duration::ZERO, responses);
    }

    /// Same as [`RouterProvider::route`], sleeping `delay` before answering.
    pub fn route_delayed(
        &self,
        marker: &str,
        key: Option<&str>,
        delay: Duration,
        responses: Vec<CompletionResponse>,
    ) {
        self.routes.lock().unwrap().push(Route {
            marker: marker.to_string(),
            key: key.map(str::to_string),
            delay,
            responses: responses.into(),
        });
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn calls_for(&self, role: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.role == role)
            .collect()
    }

    pub fn calls_with_key(&self, key: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.key.as_deref() == Some(key))
            .count()
    }

    /// Roles whose response was actually delivered (after any delay).
    pub fn completed_for(&self, role: &str) -> usize {
        self.completed
            .lock()
            .unwrap()
            .iter()
            .filter(|r| *r == role)
            .count()
    }

    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ModelProvider for RouterProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "router".into(),
            supports_tools: true,
            supports_thinking: true,
            default_model: "router".into(),
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let role = role_of(&request.system);
        let user = request
            .messages
            .first()
            .map(|m| m.text())
            .unwrap_or_default();
        let picked = {
            let mut routes = self.routes.lock().unwrap();
            routes
                .iter_mut()
                .find(|r| {
                    request.system.contains(&r.marker)
                        && r.key.as_ref().is_none_or(|k| request.system.contains(k))
                        && !r.responses.is_empty()
                })
                .map(|r| (r.key.clone(), r.delay, r.responses.pop_front().unwrap()))
        };
        self.calls.lock().unwrap().push(Call {
            role: role.clone(),
            key: picked.as_ref().and_then(|p| p.0.clone()),
            system: request.system.clone(),
            user,
        });
        let Some((_, delay, response)) = picked else {
            return Err(Error::other(format!(
                "router: no scripted response for role `{role}`"
            )));
        };
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.completed.lock().unwrap().push(role);
        Ok(response)
    }
}

/// Resolver sending every model to the router.
pub struct Resolver(pub Arc<RouterProvider>);

impl ProviderResolver for Resolver {
    fn resolve(&self, model: &ModelRef) -> Result<(Arc<dyn ModelProvider>, String)> {
        Ok((self.0.clone(), model.model.clone()))
    }
}

/// Final answer made of a JSON document in a fence.
pub fn json(value: serde_json::Value) -> CompletionResponse {
    text_response(format!("Here is my answer.\n\n```json\n{value}\n```"))
}

pub fn coder_done(summary: &str) -> CompletionResponse {
    json(serde_json::json!({
        "status": "done", "summary": summary, "files_changed": ["src/lib.rs"], "notes": ""
    }))
}

pub fn coder_failed(why: &str) -> CompletionResponse {
    json(serde_json::json!({
        "status": "failed", "summary": why, "files_changed": [], "notes": "tried"
    }))
}

pub fn qa(verdict: &str, issues: &[&str]) -> CompletionResponse {
    let issues: Vec<serde_json::Value> = issues
        .iter()
        .map(|t| serde_json::json!({"severity": "high", "title": t, "detail": "details"}))
        .collect();
    json(serde_json::json!({"verdict": verdict, "summary": "checked", "issues": issues}))
}

pub fn fixer_done() -> CompletionResponse {
    json(serde_json::json!({"status": "done", "summary": "fixed", "fixed": ["x"]}))
}

pub fn spec_json(summary: &str) -> CompletionResponse {
    json(serde_json::json!({
        "summary": summary,
        "requirements": [
            {"id": "R1", "description": "It works", "kind": "functional", "priority": 1,
             "acceptance": ["cargo test passes"]}
        ],
        "context": {"relevant_files": ["src/lib.rs"], "findings": ["tests live in tests/"],
                    "assumptions": []}
    }))
}

/// Key matching the coder prompt of the `pos`-th of `total` subtasks.
pub fn subtask_key(pos: usize, total: usize, title: &str) -> String {
    format!("### Subtask {pos}/{total}: {title}\n")
}

/// Collects every event.
#[derive(Default)]
pub struct Collector(pub Mutex<Vec<Event>>);

#[async_trait::async_trait]
impl EventSink for Collector {
    async fn on_event(&self, e: &Envelope) {
        self.0.lock().unwrap().push(e.event.clone());
    }
}

impl Collector {
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }
}

/// Hook aborting a given phase.
pub struct AbortPhase(pub Phase);

#[async_trait::async_trait]
impl vibe_core::Hook for AbortPhase {
    fn name(&self) -> &str {
        "abort-phase"
    }
    async fn before_phase(&self, phase: Phase, _task: &Task) -> HookDecision {
        if phase == self.0 {
            HookDecision::Abort("not today".into())
        } else {
            HookDecision::Continue
        }
    }
}

/// A test project with a store, a router provider and a pipeline factory.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub store: Arc<FileTaskStore>,
    pub router: Arc<RouterProvider>,
    pub events: EventBus,
    pub collector: Arc<Collector>,
    pub commits: Arc<Mutex<Vec<String>>>,
    /// Commits (`commit: <message>`) and resets (`reset`) in call order.
    pub journal: Arc<Mutex<Vec<String>>>,
    /// Plug a counting resetter into the pipeline.
    pub use_resetter: bool,
    /// When set, the committer flips this cancel token on its first call.
    pub cancel_on_commit: Arc<Mutex<Option<watch::Sender<bool>>>>,
    pub config: VibeConfig,
    pub registry: Registry,
    pub tools: ToolRegistry,
    /// Subtask workspaces handed to the pipeline.
    pub subtasks: Option<Arc<dyn vibe_core::SubtaskWorkspaces>>,
}

impl Harness {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileTaskStore::open(dir.path()).unwrap());
        let events = EventBus::default();
        let collector = Arc::new(Collector::default());
        events.add_sink(collector.clone()).await;
        let mut config = VibeConfig::default();
        config.pipeline.workspace = "in_place".into();
        Self {
            dir,
            store,
            router: RouterProvider::new(),
            events,
            collector,
            commits: Arc::new(Mutex::new(Vec::new())),
            journal: Arc::new(Mutex::new(Vec::new())),
            use_resetter: false,
            cancel_on_commit: Arc::new(Mutex::new(None)),
            config,
            registry: Registry::new(),
            tools: ToolRegistry::new(),
            subtasks: None,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub async fn task(&self, title: &str, description: &str) -> Task {
        let t = Task::new(title, description);
        self.store.save_task(&t).await.unwrap();
        t
    }

    pub fn committer(&self) -> Committer {
        let commits = self.commits.clone();
        let journal = self.journal.clone();
        let cancel = self.cancel_on_commit.clone();
        Arc::new(move |_root, message| {
            let commits = commits.clone();
            let journal = journal.clone();
            let cancel = cancel.clone();
            Box::pin(async move {
                journal.lock().unwrap().push(format!("commit: {message}"));
                commits.lock().unwrap().push(message);
                if let Some(tx) = cancel.lock().unwrap().take() {
                    let _ = tx.send(true);
                }
                Ok(Some("abc123".to_string()))
            })
        })
    }

    pub fn resetter(&self) -> Resetter {
        let journal = self.journal.clone();
        Arc::new(move |_root| {
            let journal = journal.clone();
            Box::pin(async move {
                journal.lock().unwrap().push("reset".to_string());
                Ok(())
            })
        })
    }

    pub fn resets(&self) -> usize {
        self.journal
            .lock()
            .unwrap()
            .iter()
            .filter(|e| *e == "reset")
            .count()
    }

    pub fn pipeline(&self) -> Pipeline {
        let store: Arc<dyn PipelineStore> = self.store.clone();
        Pipeline::new(PipelineDeps {
            registry: Arc::new(self.registry.clone()),
            providers: Arc::new(Resolver(self.router.clone())),
            store,
            workspace: Arc::new(InPlaceWorkspace),
            tools: self.tools.clone(),
            events: self.events.clone(),
            config: self.config.clone(),
            project_root: self.root(),
            committer: Some(self.committer()),
            resetter: self.use_resetter.then(|| self.resetter()),
            subtask_workspaces: self.subtasks.clone(),
        })
    }

    pub async fn task_dir(&self, task: &Task) -> PathBuf {
        self.store.task_dir(task.id).await.unwrap()
    }
}

/// Subtask workspaces that only record what the build asks for. Attempts
/// whose label starts with a prefix in `conflict_once` conflict on their
/// first integration.
#[derive(Default)]
pub struct FakeSubtasks {
    pub log: Mutex<Vec<String>>,
    pub conflict_once: Mutex<Vec<String>>,
}

impl FakeSubtasks {
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl vibe_core::SubtaskWorkspaces for FakeSubtasks {
    async fn open(&self, task: &vibe_core::Workspace, label: &str) -> Result<vibe_core::Workspace> {
        self.log.lock().unwrap().push(format!("open {label}"));
        let mut ws = task.clone();
        ws.root = task.root.join(format!("attempt-{label}"));
        ws.branch = Some(label.to_string());
        Ok(ws)
    }

    async fn integrate(
        &self,
        _task: &vibe_core::Workspace,
        subtask: &vibe_core::Workspace,
        _message: &str,
    ) -> Result<vibe_core::SubtaskIntegration> {
        let label = subtask.branch.clone().unwrap_or_default();
        self.log.lock().unwrap().push(format!("integrate {label}"));
        let mut once = self.conflict_once.lock().unwrap();
        if let Some(i) = once.iter().position(|p| label.starts_with(p.as_str())) {
            once.remove(i);
            return Ok(vibe_core::SubtaskIntegration::Conflict {
                files: vec!["src/lib.rs".into()],
            });
        }
        Ok(vibe_core::SubtaskIntegration::Integrated {
            commit: Some(format!("sha-{label}")),
        })
    }

    async fn discard(&self, subtask: &vibe_core::Workspace) -> Result<()> {
        let label = subtask.branch.clone().unwrap_or_default();
        self.log.lock().unwrap().push(format!("discard {label}"));
        Ok(())
    }

    async fn discard_all(&self, _task: &vibe_core::Workspace) -> Result<()> {
        self.log.lock().unwrap().push("discard_all".into());
        Ok(())
    }
}
