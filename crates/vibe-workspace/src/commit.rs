//! Committing the work an agent left in a workspace.

use std::path::Path;

use vibe_core::Result;

use crate::git::{EXCLUDE_VIBE, Git};

/// Stage every change under `workspace_root` and commit it with `message`.
///
/// The `.vibe` directory is always excluded; `exclude` lists additional
/// paths (relative to `workspace_root`) to leave out. The commit uses the
/// neutral framework identity. Returns the new commit SHA, or `Ok(None)`
/// when there was nothing to commit.
pub async fn commit_all(
    workspace_root: &Path,
    message: &str,
    exclude: &[&str],
) -> Result<Option<String>> {
    let git = Git::new(workspace_root);
    let excludes: Vec<String> = exclude
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && *p != ".vibe")
        .map(|p| format!(":(exclude){p}"))
        .collect();
    let mut add = vec!["add", "-A", "--", ".", EXCLUDE_VIBE];
    add.extend(excludes.iter().map(String::as_str));
    git.run(&add).await?;

    let staged = git.output(&["diff", "--cached", "--quiet"]).await?;
    if staged.success {
        tracing::debug!(root = %workspace_root.display(), "nothing to commit");
        return Ok(None);
    }
    git.run_as_framework(&["commit", "--quiet", "-m", message])
        .await?;
    let sha = git.head_sha().await?;
    tracing::debug!(root = %workspace_root.display(), %sha, "committed");
    Ok(Some(sha))
}

/// Whether `root` has uncommitted changes (tracked or untracked), ignoring
/// the `.vibe` directory.
pub async fn has_uncommitted(root: &Path) -> Result<bool> {
    Git::new(root).has_changes().await
}
