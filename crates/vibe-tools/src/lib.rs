//! # vibe-tools
//!
//! Built-in tools and the shell security policy used by **Vibe Factory**
//! agents.
//!
//! | Tool | Purpose | Permission | Mutating |
//! |------|---------|------------|----------|
//! | `read_file` | Read a text file with numbered lines | read | no |
//! | `write_file` | Create or overwrite a file | write | yes |
//! | `edit_file` | Exact-match replacement, returns a diff | read + write | yes |
//! | `list_dir` | Tree listing honouring `.gitignore` | read | no |
//! | `glob` | Find files by glob, newest first | read | no |
//! | `grep` | Regex search over text files | read | no |
//! | `bash` | Run a shell command after policy checks | execute | yes |
//!
//! Every path goes through `ToolContext::resolve_path`, so tools cannot
//! escape the workspace; writing tools additionally refuse extra read paths
//! and dangling symbolic links. User-level failures (missing file, denied
//! command, bad input) are returned as error outputs the model can act on;
//! `Err` is reserved for framework failures.
//!
//! ```
//! use vibe_core::config::SecurityConfig;
//!
//! let tools = vibe_tools::builtin_tools(&SecurityConfig::default());
//! assert!(tools.get("read_file").is_some());
//! assert!(tools.get("bash").is_some());
//! ```

#![forbid(unsafe_code)]

pub mod security;
pub mod text;
pub mod tools;

use std::sync::Arc;

use vibe_core::ToolRegistry;
use vibe_core::config::SecurityConfig;

pub use security::{
    CommandSegment, ParseError, Redirection, SecurityPolicy, is_probably_binary, parse_command,
    truncate_output,
};
pub use tools::{
    BashTool, EditFileTool, GlobTool, GrepTool, ListDirTool, ReadFileTool, WebFetchTool,
    WebSearchTool, WriteFileTool,
};

/// Names of the built-in tools, in registration order.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "glob",
    "grep",
    "bash",
    "web_fetch",
];

/// A registry containing every built-in tool, with `bash` configured from
/// `security`.
#[must_use]
pub fn builtin_tools(security: &SecurityConfig) -> ToolRegistry {
    let mut tools = ToolRegistry::new()
        .with(Arc::new(ReadFileTool))
        .with(Arc::new(WriteFileTool))
        .with(Arc::new(EditFileTool))
        .with(Arc::new(ListDirTool))
        .with(Arc::new(GlobTool))
        .with(Arc::new(GrepTool))
        .with(Arc::new(BashTool::new(security)))
        .with(Arc::new(WebFetchTool::new(
            security.web_allowed_domains.clone(),
        )));
    if let Some(url) = &security.search_url {
        tools.register(Arc::new(WebSearchTool::new(url.clone())));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_every_builtin_tool() {
        let reg = builtin_tools(&SecurityConfig::default());
        assert_eq!(reg.names().collect::<Vec<_>>(), BUILTIN_TOOL_NAMES);
        for tool in reg.iter() {
            let schema = tool.input_schema();
            assert_eq!(schema["type"], "object", "{}", tool.name());
            assert!(!tool.description().is_empty());
        }
        let mutating: Vec<&str> = reg
            .iter()
            .filter(|t| t.is_mutating())
            .map(|t| t.name())
            .collect();
        assert_eq!(mutating, ["write_file", "edit_file", "bash"]);
    }

    #[test]
    fn read_only_selection_matches_core() {
        let reg = builtin_tools(&SecurityConfig::default());
        let ro = vibe_core::ToolSelection::ReadOnly.resolve(reg.names());
        assert_eq!(ro, ["read_file", "list_dir", "glob", "grep", "web_fetch"]);
    }
}
