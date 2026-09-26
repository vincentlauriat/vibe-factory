//! `list_dir`: tree-like directory listing.

use serde::Deserialize;
use serde_json::json;
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

use super::common::{
    display_path, parse_input, permission_denied, resolve_read, try_output, walker, workspace_root,
};

/// Maximum number of entries listed.
pub const MAX_LIST_ENTRIES: usize = 500;
/// Depth used when none is given.
pub const DEFAULT_LIST_DEPTH: usize = 2;
/// Largest accepted depth.
pub const MAX_LIST_DEPTH: usize = 10;

/// Lists a directory as an indented tree, honouring `.gitignore`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListDirTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[async_trait::async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List a directory of the workspace as an indented tree (directories end with `/`). \
         Entries ignored by `.gitignore`, and the `.git`, `target` and `node_modules` \
         directories, are skipped. `depth` controls how many levels are shown (default 2, \
         maximum 10). At most 500 entries are returned; list a subdirectory to see more."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory to list, relative to the workspace root (default: the root)."},
                "depth": {"type": "integer", "minimum": 1, "maximum": MAX_LIST_DEPTH, "description": "Number of levels to show (default 2)."}
            },
            "additionalProperties": false
        })
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.read {
            return Ok(permission_denied("read files"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        let requested = input.path.unwrap_or_else(|| ".".into());
        let dir = try_output!(resolve_read(ctx, &requested));
        let root = workspace_root(ctx);
        let shown = display_path(&root, &dir);
        if !dir.is_dir() {
            let what = if dir.exists() {
                "is not a directory"
            } else {
                "does not exist"
            };
            return Ok(ToolOutput::error(format!("`{shown}` {what}.")));
        }
        let depth = input
            .depth
            .unwrap_or(DEFAULT_LIST_DEPTH)
            .clamp(1, MAX_LIST_DEPTH);
        let walk_dir = dir.clone();
        let (lines, truncated) = tokio::task::spawn_blocking(move || {
            let mut builder = walker(&walk_dir, Some(depth));
            builder.sort_by_file_name(std::cmp::Ord::cmp);
            let mut lines = Vec::new();
            let mut truncated = false;
            for entry in builder.build().flatten() {
                if entry.depth() == 0 {
                    continue;
                }
                if lines.len() >= MAX_LIST_ENTRIES {
                    truncated = true;
                    break;
                }
                let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
                let indent = "  ".repeat(entry.depth());
                let name = entry.file_name().to_string_lossy();
                lines.push(format!("{indent}{name}{}", if is_dir { "/" } else { "" }));
            }
            (lines, truncated)
        })
        .await
        .map_err(|e| Error::tool(format!("list_dir task failed: {e}")))?;

        let header = if shown == "." {
            "./".to_string()
        } else {
            format!("{shown}/")
        };
        let mut content = header;
        if lines.is_empty() {
            content.push_str("\n  (empty)");
        } else {
            content.push('\n');
            content.push_str(&lines.join("\n"));
        }
        if truncated {
            content.push_str(&format!(
                "\n\n(Listing truncated at {MAX_LIST_ENTRIES} entries; list a subdirectory or \
                 reduce `depth`.)"
            ));
        }
        let mut out = ToolOutput::ok(content);
        out.metadata = json!({"path": shown, "entries": lines.len(), "truncated": truncated});
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use pretty_assertions::assert_eq;

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        ListDirTool.call(ctx, input).await.unwrap()
    }

    #[tokio::test]
    async fn tree_honours_gitignore_and_skips_heavy_dirs() {
        let (dir, ctx) = workspace();
        write(&dir, ".gitignore", "*.log\nbuild/\n");
        write(&dir, "src/main.rs", "");
        write(&dir, "src/deep/x.rs", "");
        write(&dir, "debug.log", "");
        write(&dir, "build/out.o", "");
        write(&dir, "target/debug/app", "");
        write(&dir, "node_modules/pkg/index.js", "");
        write(&dir, ".git/HEAD", "");
        write(&dir, "README.md", "");
        let out = run(&ctx, json!({})).await;
        assert_eq!(
            out.content,
            "./\n  .gitignore\n  README.md\n  src/\n    deep/\n    main.rs"
        );
        let deeper = run(&ctx, json!({"path": "src", "depth": 3})).await;
        assert_eq!(deeper.content, "src/\n  deep/\n    x.rs\n  main.rs");
    }

    #[tokio::test]
    async fn caps_entries() {
        let (dir, ctx) = workspace();
        for i in 0..(MAX_LIST_ENTRIES + 20) {
            write(&dir, &format!("f{i:04}.txt"), "");
        }
        let out = run(&ctx, json!({"depth": 1})).await;
        assert!(out.content.contains("truncated at 500 entries"));
        assert_eq!(out.metadata["entries"], MAX_LIST_ENTRIES);
    }

    #[tokio::test]
    async fn errors() {
        let (dir, ctx) = workspace();
        write(&dir, "file.txt", "");
        assert!(run(&ctx, json!({"path": "file.txt"})).await.is_error);
        assert!(run(&ctx, json!({"path": "missing"})).await.is_error);
        assert!(
            run(&ctx, json!({"path": ".."}))
                .await
                .content
                .contains("Access denied")
        );
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        assert!(
            run(&ctx, json!({"path": "empty"}))
                .await
                .content
                .contains("(empty)")
        );
    }

    #[tokio::test]
    async fn absolute_path_outside_is_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let out = run(&ctx, json!({"path": outside.path().to_str().unwrap()})).await;
        assert!(out.is_error && out.content.contains("Access denied"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_directories_are_not_followed() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside-secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let out = run(&ctx, json!({"depth": 3})).await;
        assert!(!out.content.contains("outside-secret"), "{}", out.content);
        let direct = run(&ctx, json!({"path": "escape"})).await;
        assert!(direct.is_error && direct.content.contains("Access denied"));
    }
}
