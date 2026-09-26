//! Helpers shared by the built-in tools.

use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use vibe_core::{ToolContext, ToolOutput};

/// Maximum characters of output returned by `grep` and `bash`.
pub const MAX_OUTPUT_CHARS: usize = 30_000;

/// Unwrap a `Result<T, ToolOutput>`, returning the `ToolOutput` as a
/// successful tool call result on error.
macro_rules! try_output {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(output) => return Ok(output),
        }
    };
}
pub(crate) use try_output;

/// Deserialize a tool input, turning schema violations into a model-facing
/// error.
pub(crate) fn parse_input<T: DeserializeOwned>(
    tool: &str,
    input: serde_json::Value,
) -> Result<T, ToolOutput> {
    serde_json::from_value(input)
        .map_err(|e| ToolOutput::error(format!("Invalid input for `{tool}`: {e}.")))
}

/// Error returned when the agent lacks a capability.
pub(crate) fn permission_denied(capability: &str) -> ToolOutput {
    ToolOutput::error(format!(
        "Permission denied: this agent is not allowed to {capability}."
    ))
}

/// Canonical workspace root (resolving symlinks such as `/var` →
/// `/private/var` on macOS).
pub(crate) fn workspace_root(ctx: &ToolContext) -> PathBuf {
    ctx.resolve_path(".")
        .unwrap_or_else(|_| ctx.workspace_root.clone())
}

/// Resolve a path for reading (workspace or an extra read path).
pub(crate) fn resolve_read(ctx: &ToolContext, path: &str) -> Result<PathBuf, ToolOutput> {
    ctx.resolve_path(path)
        .map_err(|e| ToolOutput::error(format!("Access denied: {}.", e.message)))
}

/// Resolve a path that will be written to or used as a working directory:
/// it must be inside the workspace itself (extra read paths do not count) and
/// must not traverse a dangling symbolic link, which could otherwise redirect
/// a write outside the workspace.
pub(crate) fn resolve_inside_workspace(
    ctx: &ToolContext,
    path: &str,
) -> Result<PathBuf, ToolOutput> {
    let resolved = resolve_read(ctx, path)?;
    let root = workspace_root(ctx);
    let Ok(relative) = resolved.strip_prefix(&root) else {
        return Err(ToolOutput::error(format!(
            "Access denied: `{path}` is outside the workspace; only files inside the workspace \
             can be modified."
        )));
    };
    let mut current = root.clone();
    for component in relative.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            if std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(ToolOutput::error(format!(
                    "Access denied: `{path}` goes through a symbolic link whose target does not \
                     exist; writing through it could escape the workspace."
                )));
            }
        }
    }
    Ok(resolved)
}

/// Path shown to the model: relative to the workspace root with `/`
/// separators when possible, absolute otherwise.
pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// Directory names never descended into by the search tools.
pub(crate) const SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// A walker honouring `.gitignore` (even outside a git repository) and
/// skipping [`SKIPPED_DIRS`], independent of the user's global git excludes.
pub(crate) fn walker(root: &Path, max_depth: Option<usize>) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .max_depth(max_depth)
        .filter_entry(|entry| {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            !(is_dir
                && entry.depth() > 0
                && SKIPPED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()))
        });
    builder
}

#[cfg(test)]
pub(crate) mod test_support {
    use vibe_core::{Permissions, ToolContext};

    /// A temporary workspace with full local permissions.
    pub(crate) fn workspace() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(dir.path()).with_permissions(Permissions::local());
        (dir, ctx)
    }

    /// Write a file (creating parents) inside `dir`.
    pub(crate) fn write(dir: &tempfile::TempDir, rel: &str, content: &str) {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }
}
