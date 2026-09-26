//! `glob`: find files by glob pattern.

use std::path::Path;
use std::time::SystemTime;

use globset::GlobBuilder;
use serde::Deserialize;
use serde_json::json;
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

use super::common::{
    display_path, parse_input, permission_denied, resolve_read, try_output, walker, workspace_root,
};

/// Maximum number of paths returned.
pub const MAX_GLOB_RESULTS: usize = 2000;

/// Finds files matching a glob pattern, newest first.
#[derive(Debug, Clone, Copy, Default)]
pub struct GlobTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

/// Path relative to `base`, with `/` separators.
fn relative_slash(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files whose path matches a glob pattern, relative to the search directory \
         (default: the workspace root). `*` does not cross directories: use `**/*.rs` to \
         match at any depth, `src/**/mod.rs` for a subtree, `*.{ts,tsx}` for alternatives. \
         Files ignored by `.gitignore` and the `.git`, `target` and `node_modules` \
         directories are skipped. Results are sorted by modification time, newest first, \
         and capped at 2000."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern, e.g. `**/*.rs`."},
                "path": {"type": "string", "description": "Directory to search in, relative to the workspace root (default: the root)."}
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.read {
            return Ok(permission_denied("read files"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        let pattern = input.pattern.trim().to_string();
        if pattern.is_empty() {
            return Ok(ToolOutput::error("The glob pattern is empty."));
        }
        if pattern.starts_with('/') || pattern.split(['/', '\\']).any(|p| p == "..") {
            return Ok(ToolOutput::error(
                "The glob pattern must be relative and must not contain `..`; use `path` to \
                 choose the directory to search.",
            ));
        }
        let matcher = match GlobBuilder::new(&pattern).literal_separator(true).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return Ok(ToolOutput::error(format!("Invalid glob pattern: {e}."))),
        };
        let base = try_output!(resolve_read(ctx, input.path.as_deref().unwrap_or(".")));
        let root = workspace_root(ctx);
        if !base.is_dir() {
            return Ok(ToolOutput::error(format!(
                "`{}` is not a directory.",
                display_path(&root, &base)
            )));
        }
        let walk_base = base.clone();
        let mut found: Vec<(SystemTime, String)> = tokio::task::spawn_blocking(move || {
            walker(&walk_base, None)
                .build()
                .flatten()
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .filter(|e| matcher.is_match(relative_slash(&walk_base, e.path())))
                .map(|e| {
                    let mtime = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    (mtime, display_path(&root, e.path()))
                })
                .collect()
        })
        .await
        .map_err(|e| Error::tool(format!("glob task failed: {e}")))?;

        if found.is_empty() {
            return Ok(ToolOutput::ok(format!("No files matched `{pattern}`.")));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = found.len();
        let truncated = total > MAX_GLOB_RESULTS;
        found.truncate(MAX_GLOB_RESULTS);
        let mut content = found
            .iter()
            .map(|(_, p)| p.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            content.push_str(&format!(
                "\n\n(Showing the {MAX_GLOB_RESULTS} most recently modified of {total} matches; \
                 use a more specific pattern.)"
            ));
        }
        let mut out = ToolOutput::ok(content);
        out.metadata = json!({"matches": total, "truncated": truncated});
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        GlobTool.call(ctx, input).await.unwrap()
    }

    fn set_mtime(dir: &tempfile::TempDir, rel: &str, secs: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(dir.path().join(rel))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[tokio::test]
    async fn matches_sorted_newest_first() {
        let (dir, ctx) = workspace();
        write(&dir, "a.rs", "");
        write(&dir, "src/b.rs", "");
        write(&dir, "src/deep/c.rs", "");
        write(&dir, "notes.md", "");
        set_mtime(&dir, "a.rs", 1_000);
        set_mtime(&dir, "src/b.rs", 3_000);
        set_mtime(&dir, "src/deep/c.rs", 2_000);
        let out = run(&ctx, json!({"pattern": "**/*.rs"})).await;
        assert_eq!(out.content, "src/b.rs\nsrc/deep/c.rs\na.rs");
        let top = run(&ctx, json!({"pattern": "*.rs"})).await;
        assert_eq!(top.content, "a.rs");
        let sub = run(&ctx, json!({"pattern": "*.rs", "path": "src"})).await;
        assert_eq!(sub.content, "src/b.rs");
    }

    #[tokio::test]
    async fn honours_gitignore_and_skipped_dirs() {
        let (dir, ctx) = workspace();
        write(&dir, ".gitignore", "gen/\n");
        write(&dir, "gen/x.rs", "");
        write(&dir, "target/y.rs", "");
        write(&dir, "node_modules/z.rs", "");
        write(&dir, "ok.rs", "");
        let out = run(&ctx, json!({"pattern": "**/*.rs"})).await;
        assert_eq!(out.content, "ok.rs");
    }

    #[tokio::test]
    async fn no_match_and_invalid_patterns() {
        let (_dir, ctx) = workspace();
        let out = run(&ctx, json!({"pattern": "*.zig"})).await;
        assert!(!out.is_error && out.content.contains("No files matched"));
        for bad in ["", "../*.rs", "/etc/*", "a/[b"] {
            assert!(run(&ctx, json!({"pattern": bad})).await.is_error, "{bad}");
        }
        assert!(
            run(&ctx, json!({"pattern": "*", "path": "../"}))
                .await
                .is_error
        );
    }

    #[tokio::test]
    async fn caps_results() {
        let (dir, ctx) = workspace();
        for i in 0..(MAX_GLOB_RESULTS + 5) {
            write(&dir, &format!("f/{i}.txt"), "");
        }
        let out = run(&ctx, json!({"pattern": "**/*.txt"})).await;
        assert_eq!(out.metadata["truncated"], true);
        assert!(out.content.contains("of 2005 matches"));
    }

    #[tokio::test]
    async fn absolute_search_dir_outside_is_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let out = run(
            &ctx,
            json!({"pattern": "*", "path": outside.path().to_str().unwrap()}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Access denied"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_directories_are_not_followed() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside-secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        write(&dir, "inside.txt", "");
        let out = run(&ctx, json!({"pattern": "**/*"})).await;
        assert!(!out.content.contains("outside-secret"), "{}", out.content);
        assert!(out.content.contains("inside.txt"));
        let direct = run(&ctx, json!({"pattern": "*", "path": "escape"})).await;
        assert!(direct.is_error && direct.content.contains("Access denied"));
    }
}
