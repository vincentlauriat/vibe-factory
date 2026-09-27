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

/// Host-owned checks for the exact integration candidate, before the target is changed.
#[async_trait::async_trait]
pub trait MergeValidator: Send {
    /// Reject the integration by returning an error. The candidate must not be modified.
    async fn validate(&mut self, candidate: &Workspace) -> Result<()>;
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

    /// Prepare an integration candidate, validate it, then publish only that candidate.
    /// The target must remain unchanged if validation fails. Providers must also reject
    /// changes to the candidate or target made while the validator was running.
    /// The default fails closed; implementing `merge` alone does not support this gate.
    async fn merge_validated(
        &self,
        _workspace: &Workspace,
        _validator: &mut dyn MergeValidator,
    ) -> Result<MergeOutcome> {
        Err(crate::Error::workspace(
            "workspace provider does not support validated integration",
        ))
    }

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

/// Result of integrating a subtask's workspace into its task's workspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum SubtaskIntegration {
    /// The subtask's work is now part of the task workspace.
    Integrated {
        /// Resulting commit of the task workspace, `None` when the subtask
        /// changed nothing.
        commit: Option<String>,
    },
    /// The subtask's work conflicts with work integrated since it started.
    /// The task workspace is left exactly as it was.
    Conflict {
        /// Conflicting files.
        files: Vec<String>,
    },
}

/// Gives every subtask attempt its own workspace, forked from the current
/// state of the task workspace, and integrates finished subtasks back one
/// at a time. Parallel coder sessions then never see each other's
/// half-finished edits, and a failed attempt is thrown away without
/// touching the task workspace.
#[async_trait::async_trait]
pub trait SubtaskWorkspaces: Send + Sync {
    /// Create a workspace for one attempt, forked from the current state of
    /// `task`. `label` is unique per attempt within the task (for example
    /// `s2-a1`); leftovers of an earlier process with the same label are
    /// replaced.
    async fn open(&self, task: &Workspace, label: &str) -> Result<Workspace>;

    /// Record everything left in `subtask` with `message` and merge it into
    /// `task`. On conflict `task` must be left unchanged.
    async fn integrate(
        &self,
        task: &Workspace,
        subtask: &Workspace,
        message: &str,
    ) -> Result<SubtaskIntegration>;

    /// Remove a subtask workspace and its resources. Already-missing
    /// resources are not an error.
    async fn discard(&self, subtask: &Workspace) -> Result<()>;
}

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

    async fn merge_validated(
        &self,
        workspace: &Workspace,
        validator: &mut dyn MergeValidator,
    ) -> Result<MergeOutcome> {
        validator.validate(workspace).await?;
        Ok(MergeOutcome::NoChanges)
    }

    async fn discard(&self, _workspace: &Workspace) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LegacyProvider;

    #[async_trait::async_trait]
    impl WorkspaceProvider for LegacyProvider {
        fn name(&self) -> &str {
            "legacy"
        }
        async fn open(&self, root: &std::path::Path, task: &Task) -> Result<Workspace> {
            InPlaceWorkspace.open(root, task).await
        }
        async fn merge(&self, _: &Workspace) -> Result<MergeOutcome> {
            panic!("validated integration must not fall back to an unchecked merge")
        }
        async fn discard(&self, _: &Workspace) -> Result<()> {
            Ok(())
        }
    }

    struct UnexpectedValidator;
    #[async_trait::async_trait]
    impl MergeValidator for UnexpectedValidator {
        async fn validate(&mut self, _: &Workspace) -> Result<()> {
            panic!("legacy provider has no integration candidate")
        }
    }

    #[tokio::test]
    async fn legacy_provider_refuses_validated_integration_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = LegacyProvider
            .open(dir.path(), &Task::new("t", ""))
            .await
            .unwrap();
        let error = LegacyProvider
            .merge_validated(&workspace, &mut UnexpectedValidator)
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .contains("does not support validated integration")
        );
    }

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
