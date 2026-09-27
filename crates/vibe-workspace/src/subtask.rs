//! One git worktree per subtask attempt.
//!
//! [`GitSubtaskWorkspaces`] forks every attempt from the current commit of
//! the task branch into its own worktree and branch:
//!
//! * directory `<task worktree>--<label>` next to the task worktree (so
//!   under the git-ignored `.vibe/worktrees/`);
//! * branch `<task branch>--<label>`, for example
//!   `vibe/add-export-1a2b3c4d--s2-a1`.
//!
//! `integrate` commits what the attempt left (`.vibe` excluded, neutral
//! framework identity), then merges the subtask branch into the task
//! branch from the task worktree: fast-forward when the task branch did not
//! move, else a merge commit. A conflict aborts the merge, so the task
//! worktree is left exactly as it was, and reports the conflicting files.
//! `discard` removes the worktree and deletes its branch.

use std::path::{Path, PathBuf};

use vibe_core::{Error, Result, SubtaskIntegration, SubtaskWorkspaces, Workspace, WorkspaceKind};

use crate::commit::commit_all;
use crate::git::Git;

/// Separator between a task's worktree or branch name and a subtask label.
pub const SUBTASK_SEPARATOR: &str = "--";

/// [`SubtaskWorkspaces`] backed by git worktrees. Stateless; see the
/// [module documentation](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct GitSubtaskWorkspaces;

impl GitSubtaskWorkspaces {
    /// Provider with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Directory of the attempt `label` of the task worktree at `task_root`.
    #[must_use]
    pub fn subtask_root(task_root: &Path, label: &str) -> PathBuf {
        let name = task_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        task_root.with_file_name(format!("{name}{SUBTASK_SEPARATOR}{label}"))
    }

    /// Branch of the attempt `label` of `task_branch`.
    #[must_use]
    pub fn subtask_branch(task_branch: &str, label: &str) -> String {
        format!("{task_branch}{SUBTASK_SEPARATOR}{label}")
    }
}

fn check_label(label: &str) -> Result<()> {
    let valid = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(Error::workspace(format!(
            "invalid subtask workspace label `{label}`: use ASCII letters, digits, `-` and `_`"
        )))
    }
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::workspace(format!(
            "path is not valid UTF-8 and cannot be passed to git: {}",
            path.display()
        ))
    })
}

/// Remove the worktree at `root` and the branch `branch`, ignoring what is
/// already gone.
async fn remove(task_git: &Git, root: &Path, branch: Option<&str>) -> Result<()> {
    let root_arg = path_arg(root)?;
    // Failures are fine here: the worktree may already be gone. What matters
    // is checked below (the directory and the branch).
    let _ = task_git
        .output(&["worktree", "remove", "--force", root_arg])
        .await?;
    if tokio::fs::try_exists(root).await? {
        tokio::fs::remove_dir_all(root).await?;
    }
    task_git.worktree_prune().await?;
    if let Some(branch) = branch
        && task_git.branch_exists(branch).await?
    {
        task_git.run(&["branch", "-D", branch]).await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl SubtaskWorkspaces for GitSubtaskWorkspaces {
    async fn open(&self, task: &Workspace, label: &str) -> Result<Workspace> {
        check_label(label)?;
        let task_branch = task.branch.as_deref().ok_or_else(|| {
            Error::workspace("subtask worktrees need a task workspace on a git branch")
        })?;
        let task_git = Git::new(&task.root);
        let root = Self::subtask_root(&task.root, label);
        let branch = Self::subtask_branch(task_branch, label);
        // Leftovers of a crashed process: start over from the task branch.
        remove(&task_git, &root, Some(&branch)).await?;
        let head = task_git.head_sha().await?;
        task_git
            .run(&["worktree", "add", "-b", &branch, path_arg(&root)?, &head])
            .await?;
        tracing::debug!(root = %root.display(), %branch, %head, "created subtask worktree");
        Ok(Workspace {
            root,
            project_root: task.project_root.clone(),
            kind: WorkspaceKind::GitWorktree,
            branch: Some(branch),
            base_branch: Some(task_branch.to_string()),
        })
    }

    async fn integrate(
        &self,
        task: &Workspace,
        subtask: &Workspace,
        message: &str,
    ) -> Result<SubtaskIntegration> {
        let branch = subtask.branch.as_deref().ok_or_else(|| {
            Error::workspace("cannot integrate a subtask workspace without a branch")
        })?;
        commit_all(&subtask.root, message, &[]).await?;

        let task_git = Git::new(&task.root);
        if task_git.has_tracked_changes().await? {
            return Err(Error::workspace(format!(
                "cannot integrate {branch}: the task workspace {} has uncommitted changes",
                task.root.display()
            )));
        }
        if task_git.commits_ahead("HEAD", branch).await? == 0 {
            return Ok(SubtaskIntegration::Integrated { commit: None });
        }
        let ff = task_git.output(&["merge", "--ff-only", branch]).await?;
        if ff.success {
            return Ok(SubtaskIntegration::Integrated {
                commit: Some(task_git.head_sha().await?),
            });
        }
        let merge = task_git
            .output_as_framework(&["merge", "--no-ff", "-m", message, branch])
            .await?;
        if merge.success {
            return Ok(SubtaskIntegration::Integrated {
                commit: Some(task_git.head_sha().await?),
            });
        }
        let files = task_git.conflicted_files().await?;
        let aborted = task_git.output(&["merge", "--abort"]).await?;
        if files.is_empty() {
            let detail = if merge.stderr.trim().is_empty() {
                merge.stdout.trim().to_string()
            } else {
                merge.stderr.trim().to_string()
            };
            return Err(Error::workspace(format!(
                "git merge of {branch} into the task workspace failed: {detail}"
            )));
        }
        if !aborted.success {
            return Err(Error::workspace(format!(
                "git merge --abort failed after conflicts integrating {branch}: {}",
                aborted.stderr.trim()
            )));
        }
        tracing::debug!(%branch, ?files, "subtask integration conflicts");
        Ok(SubtaskIntegration::Conflict { files })
    }

    async fn discard(&self, subtask: &Workspace) -> Result<()> {
        if subtask.root == subtask.project_root {
            return Err(Error::workspace(
                "refusing to discard a subtask workspace whose root is the project itself",
            ));
        }
        remove(
            &Git::new(&subtask.project_root),
            &subtask.root,
            subtask.branch.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_task() {
        let root = Path::new("/p/.vibe/worktrees/add-export-1a2b3c4d");
        assert_eq!(
            GitSubtaskWorkspaces::subtask_root(root, "s2-a1"),
            Path::new("/p/.vibe/worktrees/add-export-1a2b3c4d--s2-a1")
        );
        assert_eq!(
            GitSubtaskWorkspaces::subtask_branch("vibe/add-export-1a2b3c4d", "s2-a1"),
            "vibe/add-export-1a2b3c4d--s2-a1"
        );
    }

    #[test]
    fn labels_are_restricted() {
        assert!(check_label("s1-a2").is_ok());
        for bad in ["", "../x", "a b", "s1/a1"] {
            assert!(check_label(bad).is_err(), "{bad}");
        }
    }
}
