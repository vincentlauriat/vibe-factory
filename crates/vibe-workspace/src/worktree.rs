//! Isolation through git worktrees.

use std::path::{Path, PathBuf};

use vibe_core::workspace::MergeOutcome;
use vibe_core::{Error, Result, Task, Workspace, WorkspaceKind, WorkspaceProvider};

use crate::commit::commit_all;
use crate::git::{Git, same_path};
use crate::merge_ai::{MergeStrategy, resolve_conflicts};

/// Message of the commit made in a worktree before merging.
pub const CHECKPOINT_MESSAGE: &str = "vibe: checkpoint";

/// Prefix of every branch created for a task.
pub const BRANCH_PREFIX: &str = "vibe/";

/// Git config key (under `branch.<name>`) recording the base branch a task
/// branch was created from, so that a reopened workspace merges back into
/// the right place.
const BASE_CONFIG_NAME: &str = "vibebase";

/// Workspace provider giving every task its own git worktree and branch.
///
/// See the [crate documentation](crate) for the isolation model and the
/// merge algorithm.
#[derive(Debug, Clone, Default)]
pub struct GitWorktreeProvider {
    /// Branch to fork from and merge into. `None` uses the project's current
    /// branch (or `main` / `master` when HEAD is detached).
    pub base_branch: Option<String>,
    /// Directory holding the worktrees. Relative paths are resolved against
    /// the project root. `None` means `.vibe/worktrees`.
    pub worktrees_dir: Option<PathBuf>,
    /// What to do when merging produces conflicts.
    pub merge_strategy: MergeStrategy,
}

impl GitWorktreeProvider {
    /// Provider with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Provider forking from and merging into `base_branch` when set (the
    /// semantics of `VibeConfig`'s `base_branch`).
    #[must_use]
    pub fn with_base_branch(mut self, base_branch: Option<String>) -> Self {
        self.base_branch = base_branch;
        self
    }

    /// Store worktrees in `dir` instead of `.vibe/worktrees`.
    #[must_use]
    pub fn with_worktrees_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.worktrees_dir = Some(dir.into());
        self
    }

    /// Choose how merge conflicts are handled.
    #[must_use]
    pub fn with_merge_strategy(mut self, strategy: MergeStrategy) -> Self {
        self.merge_strategy = strategy;
        self
    }

    /// Directory holding the worktrees of `project_root`.
    #[must_use]
    pub fn worktrees_root(&self, project_root: &Path) -> PathBuf {
        match &self.worktrees_dir {
            Some(dir) if dir.is_absolute() => dir.clone(),
            Some(dir) => project_root.join(dir),
            None => project_root.join(".vibe").join("worktrees"),
        }
    }

    /// Directory and branch names used for `task`: `<slug>-<short id>`.
    #[must_use]
    pub fn task_name(task: &Task) -> String {
        format!("{}-{}", task.slug(), task.id.short())
    }

    /// Branch used for `task`: `vibe/<slug>-<short id>`.
    #[must_use]
    pub fn task_branch(task: &Task) -> String {
        format!("{BRANCH_PREFIX}{}", Self::task_name(task))
    }

    async fn resolve_base(&self, git: &Git) -> Result<String> {
        match &self.base_branch {
            Some(b) if !b.trim().is_empty() => Ok(b.trim().to_string()),
            _ => git.default_branch().await,
        }
    }

    async fn base_for(&self, git: &Git, workspace: &Workspace) -> Result<String> {
        if let Some(base) = &workspace.base_branch {
            return Ok(base.clone());
        }
        if let Some(branch) = &workspace.branch
            && let Some(base) = git.config_get(&base_key(branch)).await?
        {
            return Ok(base);
        }
        self.resolve_base(git).await
    }
}

fn base_key(branch: &str) -> String {
    format!("branch.{branch}.{BASE_CONFIG_NAME}")
}

fn path_arg(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::workspace(format!(
            "path is not valid UTF-8 and cannot be passed to git: {}",
            path.display()
        ))
    })
}

/// Whether a git error message means the object was already gone.
fn is_not_found(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    [
        "not a working tree",
        "not found",
        "no such file",
        "does not exist",
        "is not a valid",
    ]
    .iter()
    .any(|needle| s.contains(needle))
}

async fn ensure_worktrees_dir(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let ignore = dir.join(".gitignore");
    if !tokio::fs::try_exists(&ignore).await? {
        tokio::fs::write(&ignore, "*\n").await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl WorkspaceProvider for GitWorktreeProvider {
    fn name(&self) -> &str {
        "git_worktree"
    }

    async fn open(&self, project_root: &Path, task: &Task) -> Result<Workspace> {
        let git = Git::new(project_root);
        if !git.is_repo().await {
            return Err(Error::workspace(format!(
                "{} is not a git repository; use the in_place workspace or run `git init`",
                project_root.display()
            )));
        }
        git.worktree_prune().await?;

        let branch = Self::task_branch(task);
        let dir = self.worktrees_root(project_root);
        ensure_worktrees_dir(&dir).await?;
        let path = dir.join(Self::task_name(task));

        let registered = git.worktree_list().await?;
        let existing = registered.iter().find(|w| {
            !same_path(&w.path, project_root)
                && (same_path(&w.path, &path) || w.branch.as_deref() == Some(branch.as_str()))
        });
        if let Some(existing) = existing {
            let root = if same_path(&existing.path, &path) {
                path
            } else {
                existing.path.clone()
            };
            let base = match git.config_get(&base_key(&branch)).await? {
                Some(base) => base,
                None => self.resolve_base(&git).await?,
            };
            tracing::debug!(root = %root.display(), %branch, "reusing worktree");
            return Ok(Workspace {
                root,
                project_root: project_root.to_path_buf(),
                kind: WorkspaceKind::GitWorktree,
                branch: Some(branch),
                base_branch: Some(base),
            });
        }

        if tokio::fs::try_exists(&path).await? {
            tracing::debug!(path = %path.display(), "removing stale unregistered worktree directory");
            tokio::fs::remove_dir_all(&path).await?;
        }

        let path_str = path_arg(&path)?;
        let base = if git.branch_exists(&branch).await? {
            git.run(&["worktree", "add", path_str, &branch]).await?;
            match git.config_get(&base_key(&branch)).await? {
                Some(base) => base,
                None => self.resolve_base(&git).await?,
            }
        } else {
            let base = self.resolve_base(&git).await?;
            git.run(&["worktree", "add", "-b", &branch, path_str, &base])
                .await?;
            base
        };
        git.config_set(&base_key(&branch), &base).await?;
        tracing::debug!(root = %path.display(), %branch, %base, "created worktree");

        Ok(Workspace {
            root: path,
            project_root: project_root.to_path_buf(),
            kind: WorkspaceKind::GitWorktree,
            branch: Some(branch),
            base_branch: Some(base),
        })
    }

    async fn merge(&self, workspace: &Workspace) -> Result<MergeOutcome> {
        let branch = workspace.branch.as_deref().ok_or_else(|| {
            Error::workspace("cannot merge a git worktree workspace without a branch")
        })?;
        let project = Git::new(&workspace.project_root);
        let base = self.base_for(&project, workspace).await?;

        if !same_path(&workspace.root, &workspace.project_root)
            && tokio::fs::try_exists(&workspace.root).await?
        {
            commit_all(&workspace.root, CHECKPOINT_MESSAGE, &[]).await?;
        }
        if !project.branch_exists(branch).await? {
            return Err(Error::workspace(format!(
                "branch {branch} does not exist in {}",
                workspace.project_root.display()
            )));
        }
        if project.has_tracked_changes().await? {
            return Err(Error::workspace(format!(
                "cannot merge {branch}: the project at {} has uncommitted changes; commit or stash them first",
                workspace.project_root.display()
            )));
        }
        if project.commits_ahead(&base, branch).await? == 0 {
            return Ok(MergeOutcome::NoChanges);
        }

        let original = project.current_branch().await?;
        if original.as_deref() != Some(base.as_str()) {
            project.checkout(&base).await?;
        }

        let ff = project.output(&["merge", "--ff-only", branch]).await?;
        if ff.success {
            let commit = project.head_sha().await?;
            tracing::debug!(%branch, %base, %commit, "fast-forward merge");
            return Ok(MergeOutcome::Merged {
                commit: Some(commit),
            });
        }

        let message = format!("Merge {branch} (vibe)");
        let merge = project
            .output_as_framework(&["merge", "--no-ff", "-m", &message, branch])
            .await?;
        if merge.success {
            let commit = project.head_sha().await?;
            tracing::debug!(%branch, %base, %commit, "merge commit");
            return Ok(MergeOutcome::Merged {
                commit: Some(commit),
            });
        }

        let conflicts = project.conflicted_files().await?;
        if conflicts.is_empty() {
            abort_and_restore(&project, original.as_deref(), &base).await;
            let detail = if merge.stderr.trim().is_empty() {
                merge.stdout.trim().to_string()
            } else {
                merge.stderr.trim().to_string()
            };
            return Err(Error::workspace(format!(
                "git merge of {branch} into {base} failed: {detail}"
            )));
        }

        if let MergeStrategy::Assisted { provider, model } = &self.merge_strategy {
            let resolved = match resolve_conflicts(
                provider.as_ref(),
                model,
                &workspace.project_root,
                &conflicts,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    abort_and_restore(&project, original.as_deref(), &base).await;
                    return Err(e);
                }
            };
            if resolved.iter().all(|r| r.resolved) {
                let mut add = vec!["add", "--"];
                add.extend(conflicts.iter().map(String::as_str));
                let committed = match project.run(&add).await {
                    Ok(_) => {
                        project
                            .run_as_framework(&["commit", "--no-edit", "--quiet"])
                            .await
                    }
                    Err(e) => Err(e),
                };
                match committed {
                    Ok(_) => {
                        let commit = project.head_sha().await?;
                        tracing::debug!(%branch, %base, %commit, "conflicts resolved by model");
                        return Ok(MergeOutcome::Merged {
                            commit: Some(commit),
                        });
                    }
                    Err(e) => {
                        abort_and_restore(&project, original.as_deref(), &base).await;
                        return Err(e);
                    }
                }
            }
        }

        abort_and_restore(&project, original.as_deref(), &base).await;
        tracing::debug!(%branch, %base, files = ?conflicts, "merge needs human review");
        Ok(MergeOutcome::NeedsHumanReview { files: conflicts })
    }

    async fn discard(&self, workspace: &Workspace) -> Result<()> {
        if same_path(&workspace.root, &workspace.project_root) {
            return Err(Error::workspace(
                "refusing to discard a workspace whose root is the project itself",
            ));
        }
        let project = Git::new(&workspace.project_root);
        let root = path_arg(&workspace.root)?;
        let removed = project
            .output(&["worktree", "remove", "--force", root])
            .await?;
        if !removed.success && !is_not_found(&removed.stderr) {
            return Err(Error::workspace(format!(
                "git worktree remove {root} failed: {}",
                removed.stderr.trim()
            )));
        }
        if tokio::fs::try_exists(&workspace.root).await? {
            tokio::fs::remove_dir_all(&workspace.root).await?;
        }
        project.worktree_prune().await?;

        if let Some(branch) = &workspace.branch {
            let deleted = project.output(&["branch", "-D", branch]).await?;
            if !deleted.success && !is_not_found(&deleted.stderr) {
                return Err(Error::workspace(format!(
                    "git branch -D {branch} failed: {}",
                    deleted.stderr.trim()
                )));
            }
        }
        Ok(())
    }

    async fn changes(&self, workspace: &Workspace) -> Result<String> {
        let git = Git::new(&workspace.root);
        let base = self
            .base_for(&Git::new(&workspace.project_root), workspace)
            .await?;
        let stat = git.diff_stat(&base, "HEAD").await?;
        let status = git.status_short().await?;
        let mut out = String::new();
        if !stat.is_empty() {
            out.push_str(&format!("Committed changes since {base}:\n{stat}\n"));
        }
        if !status.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("Uncommitted changes:\n{status}\n"));
        }
        Ok(out)
    }
}

/// Abort an in-progress merge and go back to the branch the project was on.
/// Failures are logged: this is best-effort cleanup on an error path.
async fn abort_and_restore(project: &Git, original: Option<&str>, base: &str) {
    if let Err(e) = project.run(&["merge", "--abort"]).await {
        tracing::debug!(error = %e, "git merge --abort failed");
    }
    if let Some(original) = original.filter(|o| *o != base)
        && let Err(e) = project.checkout(original).await
    {
        tracing::warn!(error = %e, branch = original, "could not restore original branch");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_convention() {
        let task = Task::new("Add OAuth login!", "");
        let name = GitWorktreeProvider::task_name(&task);
        assert!(name.starts_with("add-oauth-login-"));
        assert_eq!(name.len(), "add-oauth-login-".len() + 8);
        assert_eq!(
            GitWorktreeProvider::task_branch(&task),
            format!("vibe/{name}")
        );
    }

    #[test]
    fn worktrees_root_resolution() {
        let root = Path::new("proj");
        let p = GitWorktreeProvider::new();
        assert_eq!(p.worktrees_root(root), root.join(".vibe").join("worktrees"));
        let p = GitWorktreeProvider::new().with_worktrees_dir("wt");
        assert_eq!(p.worktrees_root(root), root.join("wt"));
    }

    #[test]
    fn not_found_detection() {
        assert!(is_not_found("fatal: '/x' is not a working tree"));
        assert!(is_not_found("error: branch 'vibe/x' not found."));
        assert!(!is_not_found("error: cannot delete branch checked out"));
    }
}
