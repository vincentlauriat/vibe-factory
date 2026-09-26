//! Workspaces: where a task's work happens.

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;
use crate::task::Task;

/// How a workspace is isolated from the main line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    /// Work directly in the project directory (no isolation).
    InPlace,
    /// A git worktree on a dedicated branch.
    GitWorktree,
    /// Provided by a plugin (container, remote sandbox, …).
    Custom(String),
}

/// A place where agents may read and write files.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    /// Root directory agents must stay inside.
    pub root: PathBuf,
    /// Project directory the workspace was created from.
    pub project_root: PathBuf,
    /// Isolation kind.
    pub kind: WorkspaceKind,
    /// Branch name, for git-based workspaces.
    #[serde(default)]
    pub branch: Option<String>,
    /// Base branch the work must eventually merge into.
    #[serde(default)]
    pub base_branch: Option<String>,
}

/// Result of integrating a workspace back into the project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum MergeOutcome {
    /// Merged cleanly.
    Merged {
        /// Resulting commit, if any.
        commit: Option<String>,
    },
    /// Nothing to merge.
    NoChanges,
    /// Conflicts remain and need a human.
    NeedsHumanReview {
        /// Conflicting files.
        files: Vec<String>,
    },
}

/// Creates, integrates and disposes of workspaces.
#[async_trait::async_trait]
pub trait WorkspaceProvider: Send + Sync {
    /// Name of the provider, for configuration.
    fn name(&self) -> &str;

    /// Create (or reopen) the workspace for a task.
    async fn open(&self, project_root: &std::path::Path, task: &Task) -> Result<Workspace>;

    /// Integrate the workspace into the project's main line.
    async fn merge(&self, workspace: &Workspace) -> Result<MergeOutcome>;

    /// Remove the workspace and its resources.
    async fn discard(&self, workspace: &Workspace) -> Result<()>;

    /// Summarise what changed in the workspace (a diff or file list) for
    /// reviewers. Default: empty.
    async fn changes(&self, _workspace: &Workspace) -> Result<String> {
        Ok(String::new())
    }
}

/// Shared handle to a workspace provider.
pub type SharedWorkspaceProvider = Arc<dyn WorkspaceProvider>;

/// Provider that works in place, without any isolation.
#[derive(Debug, Default, Clone, Copy)]
pub struct InPlaceWorkspace;

#[async_trait::async_trait]
impl WorkspaceProvider for InPlaceWorkspace {
    fn name(&self) -> &str {
        "in_place"
    }

    async fn open(&self, project_root: &std::path::Path, _task: &Task) -> Result<Workspace> {
        Ok(Workspace {
            root: project_root.to_path_buf(),
            project_root: project_root.to_path_buf(),
            kind: WorkspaceKind::InPlace,
            branch: None,
            base_branch: None,
        })
    }

    async fn merge(&self, _workspace: &Workspace) -> Result<MergeOutcome> {
        Ok(MergeOutcome::NoChanges)
    }

    async fn discard(&self, _workspace: &Workspace) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_place_uses_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let ws = InPlaceWorkspace
            .open(dir.path(), &Task::new("t", ""))
            .await
            .unwrap();
        assert_eq!(ws.root, dir.path());
        assert_eq!(ws.kind, WorkspaceKind::InPlace);
        assert_eq!(
            InPlaceWorkspace.merge(&ws).await.unwrap(),
            MergeOutcome::NoChanges
        );
    }
}
