//! `edit_file`: exact string replacement inside a file.

use serde::Deserialize;
use serde_json::json;
use vibe_core::{Result, Tool, ToolContext, ToolOutput};

use super::common::{
    display_path, parse_input, permission_denied, resolve_inside_workspace, try_output,
    workspace_root,
};
use crate::security::output::truncate_output;
use crate::text::unified_diff;

/// Maximum characters of diff returned after an edit.
pub const MAX_DIFF_CHARS: usize = 8_000;

/// Replaces an exact snippet of text in a file and returns the diff.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit an existing file by replacing an exact snippet. `old_string` must match the \
         file byte for byte, including whitespace and indentation (do not include the line \
         numbers shown by `read_file`). By default `old_string` must occur exactly once: add \
         surrounding lines to make it unique, or set `replace_all` to replace every \
         occurrence. Returns a unified diff of the change. Read the file first."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit, relative to the workspace root."},
                "old_string": {"type": "string", "description": "Exact text to replace (non-empty)."},
                "new_string": {"type": "string", "description": "Replacement text (must differ from old_string)."},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring a unique match (default false)."}
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.write || !ctx.permissions.read {
            return Ok(permission_denied("edit files"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        if input.old_string.is_empty() {
            return Ok(ToolOutput::error(
                "`old_string` must not be empty. Use `write_file` to create a file.",
            ));
        }
        if input.old_string == input.new_string {
            return Ok(ToolOutput::error(
                "`old_string` and `new_string` are identical; there is nothing to change.",
            ));
        }
        let path = try_output!(resolve_inside_workspace(ctx, &input.path));
        let shown = display_path(&workspace_root(ctx), &path);
        let original = match tokio::fs::read(&path).await {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    return Ok(ToolOutput::error(format!(
                        "`{shown}` is not valid UTF-8 text and cannot be edited."
                    )));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolOutput::error(format!(
                    "File `{shown}` does not exist. Use `write_file` to create it."
                )));
            }
            Err(e) => return Ok(ToolOutput::error(format!("Cannot read `{shown}`: {e}."))),
        };

        let (mut old, mut new) = (input.old_string, input.new_string);
        let mut count = original.matches(old.as_str()).count();
        if count == 0 && original.contains("\r\n") && !old.contains("\r\n") {
            // The file uses CRLF line endings but the snippet was given with LF.
            let crlf_old = old.replace('\n', "\r\n");
            let crlf_count = original.matches(crlf_old.as_str()).count();
            if crlf_count > 0 {
                old = crlf_old;
                new = new.replace('\n', "\r\n");
                count = crlf_count;
            }
        }
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "`old_string` was not found in `{shown}`. It must match exactly, including \
                 whitespace and indentation; read the file again and copy the text precisely."
            )));
        }
        if count > 1 && !input.replace_all {
            return Ok(ToolOutput::error(format!(
                "`old_string` occurs {count} times in `{shown}`. Include more surrounding \
                 context to make it unique, or set `replace_all` to true."
            )));
        }
        let updated = if input.replace_all {
            original.replace(old.as_str(), &new)
        } else {
            original.replacen(old.as_str(), &new, 1)
        };
        if let Err(e) = tokio::fs::write(&path, updated.as_bytes()).await {
            return Ok(ToolOutput::error(format!("Cannot write `{shown}`: {e}.")));
        }
        let diff = truncate_output(&unified_diff(&original, &updated, &shown), MAX_DIFF_CHARS);
        let noun = if count == 1 {
            "occurrence"
        } else {
            "occurrences"
        };
        let mut out = ToolOutput::ok(format!(
            "Edited `{shown}`: replaced {count} {noun}.\n\n{diff}"
        ));
        out.metadata = json!({"path": shown, "replacements": count});
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use vibe_core::Permissions;

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        EditFileTool.call(ctx, input).await.unwrap()
    }

    fn read(dir: &tempfile::TempDir, rel: &str) -> String {
        std::fs::read_to_string(dir.path().join(rel)).unwrap()
    }

    #[tokio::test]
    async fn unique_replacement_with_diff() {
        let (dir, ctx) = workspace();
        write(&dir, "src/lib.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
        let out = run(
            &ctx,
            json!({"path": "src/lib.rs", "old_string": "fn b() {}", "new_string": "fn b() { todo!() }"}),
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content
                .starts_with("Edited `src/lib.rs`: replaced 1 occurrence.")
        );
        assert!(out.content.contains("--- a/src/lib.rs"));
        assert!(out.content.contains("-fn b() {}"));
        assert!(out.content.contains("+fn b() { todo!() }"));
        assert_eq!(
            read(&dir, "src/lib.rs"),
            "fn a() {}\nfn b() { todo!() }\nfn c() {}\n"
        );
    }

    #[tokio::test]
    async fn ambiguous_and_missing_matches() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "x = 1\nx = 1\n");
        let out = run(
            &ctx,
            json!({"path": "f.txt", "old_string": "x = 1", "new_string": "x = 2"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("occurs 2 times"));
        let out = run(
            &ctx,
            json!({"path": "f.txt", "old_string": "y", "new_string": "z"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("not found"));
        assert_eq!(read(&dir, "f.txt"), "x = 1\nx = 1\n");
    }

    #[tokio::test]
    async fn replace_all() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "a a a");
        let out = run(
            &ctx,
            json!({"path": "f.txt", "old_string": "a", "new_string": "b", "replace_all": true}),
        )
        .await;
        assert!(out.content.contains("replaced 3 occurrences"));
        assert_eq!(read(&dir, "f.txt"), "b b b");
    }

    #[tokio::test]
    async fn crlf_files_accept_lf_snippets() {
        let (dir, ctx) = workspace();
        write(&dir, "w.txt", "one\r\ntwo\r\nthree\r\n");
        let out = run(
            &ctx,
            json!({"path": "w.txt", "old_string": "one\ntwo", "new_string": "1\n2"}),
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(read(&dir, "w.txt"), "1\r\n2\r\nthree\r\n");
    }

    #[tokio::test]
    async fn rejects_degenerate_input() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "abc");
        for input in [
            json!({"path": "f.txt", "old_string": "", "new_string": "x"}),
            json!({"path": "f.txt", "old_string": "a", "new_string": "a"}),
            json!({"path": "missing.txt", "old_string": "a", "new_string": "b"}),
            json!({"path": "f.txt", "old_string": "a"}),
        ] {
            assert!(run(&ctx, input).await.is_error);
        }
        std::fs::write(dir.path().join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        let out = run(
            &ctx,
            json!({"path": "bin", "old_string": "a", "new_string": "b"}),
        )
        .await;
        assert!(out.content.contains("UTF-8"));
    }

    #[tokio::test]
    async fn permissions_and_escapes() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "abc");
        let ro = ctx.clone().with_permissions(Permissions::read_only());
        let out = run(
            &ro,
            json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Permission denied"));
        let out = run(
            &ctx,
            json!({"path": "../f.txt", "old_string": "a", "new_string": "b"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Access denied"));
    }

    #[tokio::test]
    async fn absolute_path_outside_is_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("f.txt");
        std::fs::write(&target, "abc").unwrap();
        let out = run(
            &ctx,
            json!({"path": target.to_str().unwrap(), "old_string": "a", "new_string": "b"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Access denied"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "abc");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_outside_is_denied() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("f.txt");
        std::fs::write(&target, "abc").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("link.txt")).unwrap();
        let out = run(
            &ctx,
            json!({"path": "link.txt", "old_string": "a", "new_string": "b"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Access denied"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "abc");
    }
}
