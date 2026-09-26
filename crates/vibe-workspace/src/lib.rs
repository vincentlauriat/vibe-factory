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
//! [`InPlaceWorkspace`] (`"in_place"`) works directly in the project
//! directory, without isolation.
//!
//! ## Merge algorithm
//!
//! [`GitWorktreeProvider`]'s `merge`:
//!
//! 1. commits anything left uncommitted in the worktree (`vibe: checkpoint`,
//!    `.vibe` excluded, neutral framework identity);
//! 2. refuses to proceed if tracked files of the project checkout have
//!    uncommitted changes;
//! 3. returns [`MergeOutcome::NoChanges`] when the task branch has no commit
//!    ahead of the base;
//! 4. checks out the base branch and tries `git merge --ff-only`, then
//!    `git merge --no-ff`;
//! 5. on conflicts, with [`MergeStrategy::Assisted`], asks a model to resolve
//!    each file ([`merge_ai::resolve_conflicts`]) and commits if every file
//!    was resolved; otherwise aborts the merge, returns to the branch the
//!    project was on, and reports [`MergeOutcome::NeedsHumanReview`] with the
//!    conflicting files.
//!
//! `discard` removes the worktree (`git worktree remove --force`) and deletes
//! the task branch; already-missing resources are not an error.
//!
//! All git access goes through the `git` executable ([`git::Git`]), with
//! `GIT_TERMINAL_PROMPT=0` so nothing ever blocks on a prompt.

pub mod commit;
pub mod git;
pub mod in_place;
pub mod merge_ai;
pub mod worktree;

use std::sync::Arc;

pub use commit::{commit_all, has_uncommitted};
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
