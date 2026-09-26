//! Persistence of tasks and their artefacts.

use std::sync::Arc;

use crate::error::Result;
use crate::ids::TaskId;
use crate::plan::Plan;
use crate::qa::QaReport;
use crate::spec::Spec;
use crate::task::Task;

/// Persists tasks, specs, plans and QA reports.
///
/// The default implementation writes JSON and markdown files under
/// `.vibe/tasks/`; plugins may provide databases or remote stores.
#[async_trait::async_trait]
pub trait TaskStore: Send + Sync {
    /// Create or update a task.
    async fn save_task(&self, task: &Task) -> Result<()>;
    /// Load a task.
    async fn load_task(&self, id: TaskId) -> Result<Task>;
    /// Every task, newest first.
    async fn list_tasks(&self) -> Result<Vec<Task>>;
    /// Delete a task and all its artefacts.
    async fn delete_task(&self, id: TaskId) -> Result<()>;

    /// Save the spec of a task.
    async fn save_spec(&self, spec: &Spec) -> Result<()>;
    /// Load the spec of a task, if any.
    async fn load_spec(&self, id: TaskId) -> Result<Option<Spec>>;

    /// Save the plan of a task.
    async fn save_plan(&self, plan: &Plan) -> Result<()>;
    /// Load the plan of a task, if any.
    async fn load_plan(&self, id: TaskId) -> Result<Option<Plan>>;

    /// Save a QA report (one per round).
    async fn save_qa_report(&self, report: &QaReport) -> Result<()>;
    /// Load every QA report of a task, oldest first.
    async fn load_qa_reports(&self, id: TaskId) -> Result<Vec<QaReport>>;

    /// Append free-form progress notes for humans and future agents.
    async fn append_progress(&self, id: TaskId, note: &str) -> Result<()>;
    /// Read the progress notes.
    async fn load_progress(&self, id: TaskId) -> Result<String>;
}

/// Shared handle to a task store.
pub type SharedTaskStore = Arc<dyn TaskStore>;
