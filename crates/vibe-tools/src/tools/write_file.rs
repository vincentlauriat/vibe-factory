//! `write_file`: create or overwrite a file.

use serde::Deserialize;
use serde_json::json;
use vibe_core::{Result, Tool, ToolContext, ToolOutput};

use super::common::{
    display_path, parse_input, permission_denied, resolve_inside_workspace, try_output,
    workspace_root,
};

/// Creates or overwrites a file inside the workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    content: String,
}

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write a file inside the workspace, replacing its whole content. Missing parent \
         directories are created. Use this to create new files; to change part of an existing \
         file prefer `edit_file`, which is safer and cheaper. Reports the number of bytes \
         written and whether the file was created or overwritten."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to write, relative to the workspace root."},
                "content": {"type": "string", "description": "Complete new content of the file."}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.write {
            return Ok(permission_denied("write files"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        let path = try_output!(resolve_inside_workspace(ctx, &input.path));
        let shown = display_path(&workspace_root(ctx), &path);
        let existed = match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_dir() => {
                return Ok(ToolOutput::error(format!(
                    "`{shown}` is a directory and cannot be written as a file."
                )));
            }
            Ok(_) => true,
            Err(_) => false,
        };
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(ToolOutput::error(format!(
                "Cannot create the parent directories of `{shown}`: {e}."
            )));
        }
        if let Err(e) = tokio::fs::write(&path, input.content.as_bytes()).await {
            return Ok(ToolOutput::error(format!("Cannot write `{shown}`: {e}.")));
        }
        let bytes = input.content.len();
        let verb = if existed { "Overwrote" } else { "Created" };
        let mut out = ToolOutput::ok(format!("{verb} `{shown}` ({bytes} bytes)."));
        out.metadata = json!({"path": shown, "bytes": bytes, "created": !existed});
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::workspace;
    use vibe_core::Permissions;

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        WriteFileTool.call(ctx, input).await.unwrap()
    }

    #[tokio::test]
    async fn creates_then_overwrites() {
        let (dir, ctx) = workspace();
        let out = run(&ctx, json!({"path": "a/b/c.txt", "content": "hello"})).await;
        assert_eq!(out.content, "Created `a/b/c.txt` (5 bytes).");
        assert_eq!(out.metadata["created"], true);
        let out = run(&ctx, json!({"path": "a/b/c.txt", "content": "héllo"})).await;
        assert_eq!(out.content, "Overwrote `a/b/c.txt` (6 bytes).");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "héllo"
        );
    }

    #[tokio::test]
    async fn refuses_directories_and_bad_input() {
        let (dir, ctx) = workspace();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        assert!(
            run(&ctx, json!({"path": "d", "content": "x"}))
                .await
                .is_error
        );
        assert!(run(&ctx, json!({"path": "x"})).await.is_error);
    }

    #[tokio::test]
    async fn requires_write_permission() {
        let (dir, ctx) = workspace();
        let ctx = ctx.with_permissions(Permissions::read_only());
        let out = run(&ctx, json!({"path": "x.txt", "content": "x"})).await;
        assert!(out.is_error && out.content.contains("Permission denied"));
        assert!(!dir.path().join("x.txt").exists());
    }

    #[tokio::test]
    async fn path_escapes_are_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let abs = outside.path().join("pwned.txt");
        for p in ["../pwned.txt", "sub/../../pwned.txt", abs.to_str().unwrap()] {
            let out = run(&ctx, json!({"path": p, "content": "x"})).await;
            assert!(out.is_error, "{p} should be denied");
        }
        assert!(!abs.exists());
    }

    #[tokio::test]
    async fn extra_read_paths_are_not_writable() {
        let (_dir, ctx) = workspace();
        let extra = tempfile::tempdir().unwrap();
        let mut perms = Permissions::local();
        perms.extra_read_paths.push(extra.path().to_path_buf());
        let ctx = ctx.with_permissions(perms);
        let target = extra.path().join("f.txt");
        let out = run(
            &ctx,
            json!({"path": target.to_str().unwrap(), "content": "x"}),
        )
        .await;
        assert!(out.is_error && out.content.contains("outside the workspace"));
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_redirect_writes_outside() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        // Existing directory reached through a link.
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        let out = run(&ctx, json!({"path": "out/f.txt", "content": "x"})).await;
        assert!(out.is_error);
        assert!(!outside.path().join("f.txt").exists());
        // Dangling link to a file that does not exist yet.
        let target = outside.path().join("new.txt");
        std::os::unix::fs::symlink(&target, dir.path().join("dangling")).unwrap();
        let out = run(&ctx, json!({"path": "dangling", "content": "x"})).await;
        assert!(
            out.is_error && out.content.contains("symbolic link"),
            "{}",
            out.content
        );
        assert!(!target.exists());
        // A link staying inside the workspace is fine.
        crate::tools::common::test_support::write(&dir, "real/a.txt", "a");
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        let out = run(&ctx, json!({"path": "alias/b.txt", "content": "b"})).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(dir.path().join("real/b.txt").exists());
    }
}
