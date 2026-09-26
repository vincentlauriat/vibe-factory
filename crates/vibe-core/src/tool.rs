//! Tools: capabilities offered to agents.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::error::Result;
use crate::ids::TaskId;
use crate::provider::ToolSpec;

/// What a tool invocation is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Permissions {
    /// May read files under the workspace root.
    pub read: bool,
    /// May create or modify files under the workspace root.
    pub write: bool,
    /// May execute shell commands (subject to the command allowlist).
    pub execute: bool,
    /// May reach the network.
    pub network: bool,
    /// Extra paths (outside the workspace) that may be read.
    #[serde(default)]
    pub extra_read_paths: Vec<PathBuf>,
}

impl Permissions {
    /// Read-only access.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
            network: false,
            extra_read_paths: vec![],
        }
    }

    /// Full local access, no network.
    #[must_use]
    pub fn local() -> Self {
        Self {
            read: true,
            write: true,
            execute: true,
            network: false,
            extra_read_paths: vec![],
        }
    }

    /// Everything.
    #[must_use]
    pub fn all() -> Self {
        Self {
            read: true,
            write: true,
            execute: true,
            network: true,
            extra_read_paths: vec![],
        }
    }
}

impl Default for Permissions {
    fn default() -> Self {
        Self::local()
    }
}

/// Context handed to a tool for one invocation.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Root directory the tool must stay inside.
    pub workspace_root: PathBuf,
    /// Permissions granted to this invocation.
    pub permissions: Permissions,
    /// Task being worked on, if any.
    pub task_id: Option<TaskId>,
    /// Name of the agent calling the tool.
    pub agent: String,
}

impl ToolContext {
    /// Context rooted at `root` with default permissions.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: root.into(),
            permissions: Permissions::default(),
            task_id: None,
            agent: String::from("anonymous"),
        }
    }

    /// Override permissions.
    #[must_use]
    pub fn with_permissions(mut self, permissions: Permissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// Resolve a user-supplied path against the workspace root and verify it
    /// stays inside the root (or an allowed extra read path).
    ///
    /// Symlinks are resolved for the existing prefix of the path so that a
    /// link pointing outside the workspace is rejected.
    pub fn resolve_path(&self, input: impl AsRef<Path>) -> Result<PathBuf> {
        let input = input.as_ref();
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.workspace_root.join(input)
        };
        let canonical = canonicalize_lenient(&joined).ok_or_else(|| {
            crate::Error::denied(format!(
                "path `{}` goes through a dangling symbolic link",
                input.display()
            ))
        })?;
        let root = canonicalize_lenient(&self.workspace_root)
            .unwrap_or_else(|| self.workspace_root.clone());
        if canonical.starts_with(&root) {
            return Ok(canonical);
        }
        for extra in &self.permissions.extra_read_paths {
            if canonicalize_lenient(extra).is_some_and(|e| canonical.starts_with(e)) {
                return Ok(canonical);
            }
        }
        Err(crate::Error::denied(format!(
            "path `{}` escapes the workspace `{}`",
            input.display(),
            self.workspace_root.display()
        )))
    }
}

/// Canonicalize the longest existing prefix of `path` and re-append the rest,
/// after normalising `.` and `..` components lexically.
///
/// Returns `None` when the existing prefix cannot be canonicalised, which
/// happens when it goes through a dangling symbolic link: such a path must be
/// treated as escaping the workspace because the link target is unknown.
fn canonicalize_lenient(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    // 1. Lexical normalisation.
    let mut normalised = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                normalised.pop();
            }
            other => normalised.push(other.as_os_str()),
        }
    }
    // 2. Split into an existing prefix and a non-existing suffix.
    let mut existing = normalised.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while existing.symlink_metadata().is_err() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    let mut out = if existing.as_os_str().is_empty() {
        existing
    } else {
        existing.canonicalize().ok()?
    };
    for name in rest.iter().rev() {
        out.push(name);
    }
    Some(out)
}

/// Output of a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    /// Text returned to the model.
    pub content: String,
    /// Whether the call failed (the content then explains why).
    #[serde(default)]
    pub is_error: bool,
    /// Structured metadata for observers (not shown to the model).
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ToolOutput {
    /// Successful output.
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: serde_json::Value::Null,
        }
    }

    /// Failed output.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            metadata: serde_json::Value::Null,
        }
    }
}

/// A capability an agent can invoke.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique name, `snake_case`.
    fn name(&self) -> &str;

    /// Description shown to the model.
    fn description(&self) -> &str;

    /// JSON schema of the input object.
    fn input_schema(&self) -> serde_json::Value;

    /// Whether the tool mutates the workspace (used for permission checks and
    /// for deciding whether calls may run in parallel).
    fn is_mutating(&self) -> bool {
        false
    }

    /// Execute the tool.
    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput>;

    /// Specification advertised to the model.
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Shared handle to a tool.
pub type SharedTool = Arc<dyn Tool>;

/// An ordered collection of tools.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: IndexMap<String, SharedTool>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

impl ToolRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool, replacing any tool with the same name.
    pub fn register(&mut self, tool: SharedTool) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Builder-style registration.
    #[must_use]
    pub fn with(mut self, tool: SharedTool) -> Self {
        self.register(tool);
        self
    }

    /// Look a tool up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SharedTool> {
        self.tools.get(name)
    }

    /// Names in registration order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Number of tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Specs of every tool, for a completion request.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    /// A new registry containing only the named tools (unknown names are
    /// ignored).
    #[must_use]
    pub fn subset<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut out = Self::new();
        for n in names {
            if let Some(t) = self.tools.get(n) {
                out.register(Arc::clone(t));
            }
        }
        out
    }

    /// Merge another registry into this one.
    pub fn extend(&mut self, other: &ToolRegistry) {
        for t in other.tools.values() {
            self.register(Arc::clone(t));
        }
    }

    /// Iterate over tools.
    pub fn iter(&self) -> impl Iterator<Item = &SharedTool> {
        self.tools.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
            Ok(ToolOutput::ok(input.to_string()))
        }
    }

    #[tokio::test]
    async fn registry_and_subset() {
        let reg = ToolRegistry::new().with(Arc::new(Echo));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.subset(["echo", "nope"]).len(), 1);
        let out = reg
            .get("echo")
            .unwrap()
            .call(&ToolContext::new("."), serde_json::json!(1))
            .await
            .unwrap();
        assert_eq!(out.content, "1");
    }

    #[test]
    fn path_containment() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        assert!(ctx.resolve_path("src/main.rs").is_ok());
        assert!(ctx.resolve_path("../outside").is_err());
        assert!(ctx.resolve_path("/etc/passwd").is_err());
        let inner = ctx.resolve_path("a/../b").unwrap();
        assert!(inner.ends_with("b"));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("missing.txt"), dir.path().join("link"))
            .unwrap();
        let ctx = ToolContext::new(dir.path());
        assert!(ctx.resolve_path("link").is_err());
        assert!(ctx.resolve_path("link/child").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("real.txt"), dir.path().join("link"))
            .unwrap();
        let ctx = ToolContext::new(dir.path());
        assert!(ctx.resolve_path("link").is_err());
    }

    #[test]
    fn extra_read_paths_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let mut perms = Permissions::read_only();
        perms.extra_read_paths.push(extra.path().to_path_buf());
        let ctx = ToolContext::new(dir.path()).with_permissions(perms);
        assert!(ctx.resolve_path(extra.path().join("x")).is_ok());
    }
}
