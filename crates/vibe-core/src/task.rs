//! Tasks: the unit of work handed to the pipeline.

use chrono::{DateTime, Utc};

use crate::ids::TaskId;

/// Lifecycle state of a task on the board.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Created, not started.
    #[default]
    Backlog,
    /// Spec and plan are being produced.
    Planning,
    /// Subtasks are being implemented.
    Building,
    /// QA is reviewing or fixing.
    Review,
    /// Waiting for a human to review and merge.
    Ready,
    /// Merged / accepted.
    Done,
    /// The pipeline gave up.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

impl TaskStatus {
    /// Whether the task is in a terminal state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Complexity class of a task, used to pick a pipeline profile.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    /// One-line change, no spec needed.
    Trivial,
    /// Small, well understood change; a light spec is enough.
    Simple,
    /// Typical feature; full spec, plan and QA.
    Standard,
    /// Cross-cutting change; full pipeline with research and critique.
    Complex,
}

impl Complexity {
    /// Whether this complexity warrants writing a full specification.
    #[must_use]
    pub fn needs_spec(self) -> bool {
        self >= Complexity::Simple
    }

    /// Whether this complexity warrants the research and critique steps.
    #[must_use]
    pub fn needs_research(self) -> bool {
        self >= Complexity::Standard
    }
}

/// Where a task came from.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TaskSource {
    /// Typed by a user.
    #[default]
    Manual,
    /// Imported from an issue tracker.
    Issue {
        /// Provider name (github, gitlab, jira, …).
        provider: String,
        /// Provider-specific reference (number, key, URL).
        reference: String,
    },
    /// Produced by another task or an automated discovery run.
    Derived {
        /// Parent task.
        parent: TaskId,
    },
}

/// A unit of work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Task {
    /// Unique identifier.
    pub id: TaskId,
    /// Short title.
    pub title: String,
    /// Full description written by the user.
    pub description: String,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// Complexity, once assessed.
    pub complexity: Option<Complexity>,
    /// Free-form labels.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Origin of the task.
    #[serde(default)]
    pub source: TaskSource,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last modification timestamp.
    pub updated_at: DateTime<Utc>,
    /// Branch of the task workspace, recorded when a run opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl Task {
    /// Create a new task in the backlog.
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: TaskId::new(),
            title: title.into(),
            description: description.into(),
            status: TaskStatus::Backlog,
            complexity: None,
            labels: Vec::new(),
            source: TaskSource::Manual,
            created_at: now,
            updated_at: now,
            branch: None,
        }
    }

    /// Slug derived from the title, safe for file and branch names.
    #[must_use]
    pub fn slug(&self) -> String {
        slugify(&self.title)
    }

    /// Move the task to a new status and bump `updated_at`.
    pub fn set_status(&mut self, status: TaskStatus) {
        self.status = status;
        self.touch();
    }

    /// Bump `updated_at`.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

/// Turn free text into a lowercase, hyphen-separated slug (max 48 chars).
#[must_use]
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_safe() {
        assert_eq!(
            slugify("Add OAuth login (GitHub)!"),
            "add-oauth-login-github"
        );
        assert_eq!(slugify("   "), "task");
        assert!(slugify(&"x".repeat(200)).len() <= 48);
    }

    #[test]
    fn complexity_thresholds() {
        assert!(!Complexity::Trivial.needs_spec());
        assert!(Complexity::Simple.needs_spec());
        assert!(!Complexity::Simple.needs_research());
        assert!(Complexity::Complex.needs_research());
    }

    #[test]
    fn task_json_roundtrip() {
        let t = Task::new("Title", "Body");
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
