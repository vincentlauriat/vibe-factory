//! # vibe-workspace
//!
//! Workspace isolation and merge for **Vibe Factory**. This crate implements
//! [`vibe_core::WorkspaceProvider`], the extension point that decides *where*
//! a task's agents read and write files and how their work reaches the
//! project's main line.
//!
//! ## Isolation model
//!
//! [`GitWorktreeProvider`] (`"git_worktree"`, the default) gives every task
//! its own [git worktree](https://git-scm.com/docs/git-worktree):
//!
//! * directory `<project>/.vibe/worktrees/<task-slug>-<short-id>`;
//! * branch `vibe/<task-slug>-<short-id>`, forked from the base branch (the
//!   configured `base_branch`, else the project's current branch);
//! * `.vibe/worktrees/` holds a `.gitignore` containing `*`, so worktrees
//!   never show up in the project's own status;
//! * reopening a task reuses its registered worktree; a stale directory that
//!   git no longer knows about is removed and recreated, and
//!   `git worktree prune` runs first to forget worktrees whose directory
//!   vanished;
//! * the base branch is recorded in the repository config
//!   (`branch.<branch>.vibebase`) so a reopened workspace merges back into
//!   the branch it came from.
//!
//! The project checkout is never touched while agents work, so a human can
//! keep using it and several tasks can run in parallel.
//!
//! ## Untrusted repository configuration
//!
//! Worktrees share the repository configuration with the user's checkout,
//! and agents can run `git config`. Two defences apply:
//!
//! * every git command the framework runs overrides the helpers that would
//!   execute code (hooks, fsmonitor, pager, editor, SSH command) and ignores
//!   the system-wide config (see [`git`]);
//! * `open` snapshots the project's repository-local configuration
//!   (`git config --local --list`, plus `--worktree` when enabled) into
//!   memory and into `<worktrees dir>/.snapshots/<task>.cfg`, never
//!   overwriting an existing snapshot. `merge` refuses to proceed if any key
//!   changed, except the explicitly volatile ones
//!   ([`config_guard::is_volatile_key`]), and lists the changed keys. After a
//!   human review, [`GitWorktreeProvider::accept_config_changes`] records the
//!   current configuration as the new baseline.
//!
//! [`InPlaceWorkspace`] (`"in_place"`) works directly in the project
//! directory, without isolation.
//!
//! ## Merge algorithm
//!
//! [`GitWorktreeProvider`]'s `merge`:
//!
//! 1. refuses if the repository configuration changed since `open` (see
//!    above);
//! 2. commits anything left uncommitted in the worktree (`vibe: checkpoint`,
//!    `.vibe` excluded, neutral framework identity);
//! 3. refuses to proceed if tracked files of the project checkout have
//!    uncommitted changes;
//! 4. returns [`MergeOutcome::NoChanges`] when the task branch has no commit
//!    ahead of the base;
//! 5. checks out the base branch and tries `git merge --ff-only`, then
//!    `git merge --no-ff`;
//! 6. on conflicts, with [`MergeStrategy::Assisted`], asks a model to resolve
//!    each file ([`merge_ai::resolve_conflicts`]) and commits if every file
//!    was resolved; otherwise aborts the merge, returns to the branch the
//!    project was on, and reports [`MergeOutcome::NeedsHumanReview`] with the
//!    conflicting files.
//!
//! With required checks, the pipeline calls `merge_validated` instead: it prepares a
//! detached integration worktree, resolves conflicts there, invokes the host validator,
//! checks that the candidate and target stayed unchanged, and fast-forwards the target
//! to the exact tested commit. Validation failure never publishes the candidate.
//!
//! `discard` removes the worktree (`git worktree remove --force`) and deletes
//! the task branch; already-missing resources are not an error.
//!
//! All git access goes through the `git` executable ([`git::Git`]), with
//! `GIT_TERMINAL_PROMPT=0` so nothing ever blocks on a prompt.

pub mod commit;
pub mod config_guard;
pub mod git;
pub mod in_place;
pub mod merge_ai;
pub mod worktree;

use std::sync::Arc;

pub use commit::{commit_all, has_uncommitted};
pub use config_guard::{ChangeKind, ConfigChange, ConfigSnapshot};
pub use git::{Git, GitOutput, WorktreeInfo};
pub use in_place::InPlaceWorkspace;
pub use merge_ai::{MergeStrategy, Resolution, resolve_conflicts};
pub use vibe_core::workspace::{MergeOutcome, SharedWorkspaceProvider};
pub use worktree::GitWorktreeProvider;

/// Build a built-in workspace provider from its configured name:
/// `"git_worktree"` (using `base_branch`) or `"in_place"`. Returns `None`
/// for any other name, which a plugin may provide instead.
#[must_use]
pub fn provider_by_name(
    name: &str,
    base_branch: Option<String>,
) -> Option<SharedWorkspaceProvider> {
    match name {
        "git_worktree" => Some(Arc::new(
            GitWorktreeProvider::new().with_base_branch(base_branch),
        )),
        "in_place" => Some(Arc::new(InPlaceWorkspace)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_by_name() {
        assert_eq!(
            provider_by_name("git_worktree", None).unwrap().name(),
            "git_worktree"
        );
        assert_eq!(
            provider_by_name("in_place", None).unwrap().name(),
            "in_place"
        );
        assert!(provider_by_name("docker", None).is_none());
    }
}
