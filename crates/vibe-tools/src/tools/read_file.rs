//! `read_file`: read a text file with line numbers.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde::Deserialize;
use serde_json::json;
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

use super::common::{
    display_path, parse_input, permission_denied, resolve_read, try_output, workspace_root,
};
use crate::security::output::{BINARY_SNIFF_BYTES, is_probably_binary};
use crate::text::{MAX_LINE_CHARS, number_line, truncate_line};

/// Lines returned when no `limit` is given.
pub const DEFAULT_READ_LIMIT: usize = 2000;
/// Largest accepted `limit`.
pub const MAX_READ_LIMIT: usize = 10_000;

/// Reads a text file and returns its lines numbered like `cat -n`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadFileTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

struct Excerpt {
    lines: Vec<String>,
    total_lines: usize,
    binary: bool,
}

fn read_excerpt(path: &Path, offset: usize, limit: usize) -> std::io::Result<Excerpt> {
    let mut file = std::fs::File::open(path)?;
    let mut sniff = vec![0u8; BINARY_SNIFF_BYTES];
    let n = read_up_to(&mut file, &mut sniff)?;
    sniff.truncate(n);
    if is_probably_binary(&sniff) {
        return Ok(Excerpt {
            lines: Vec::new(),
            total_lines: 0,
            binary: true,
        });
    }
    let mut reader = BufReader::new(std::io::Cursor::new(sniff).chain(file));
    let mut lines = Vec::new();
    let mut total = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        total += 1;
        if total >= offset && lines.len() < limit {
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim_end_matches('\n').trim_end_matches('\r');
            lines.push(text.to_string());
        }
    }
    Ok(Excerpt {
        lines,
        total_lines: total,
        binary: false,
    })
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file from the workspace. Lines are returned numbered like `cat -n` \
         (six-column line number, a tab, then the line). By default the first 2000 lines are \
         returned; use `offset` (1-based first line) and `limit` (number of lines) to page \
         through longer files. Lines longer than 2000 characters are truncated. Binary files \
         are refused. Paths are relative to the workspace root."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to read, relative to the workspace root."},
                "offset": {"type": "integer", "minimum": 1, "description": "1-based line number to start from (default 1)."},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LIMIT, "description": "Maximum number of lines to return (default 2000)."}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.read {
            return Ok(permission_denied("read files"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        let path = try_output!(resolve_read(ctx, &input.path));
        let shown = display_path(&workspace_root(ctx), &path);
        match std::fs::metadata(&path) {
            Err(_) => {
                return Ok(ToolOutput::error(format!("File `{shown}` does not exist.")));
            }
            Ok(m) if m.is_dir() => {
                return Ok(ToolOutput::error(format!(
                    "`{shown}` is a directory; use `list_dir` to see its contents."
                )));
            }
            Ok(_) => {}
        }
        let offset = input.offset.unwrap_or(1).max(1);
        let limit = input
            .limit
            .unwrap_or(DEFAULT_READ_LIMIT)
            .clamp(1, MAX_READ_LIMIT);
        let read_path = path.clone();
        let excerpt = tokio::task::spawn_blocking(move || read_excerpt(&read_path, offset, limit))
            .await
            .map_err(|e| Error::tool(format!("read_file task failed: {e}")))?;
        let excerpt = match excerpt {
            Ok(e) => e,
            Err(e) => return Ok(ToolOutput::error(format!("Cannot read `{shown}`: {e}."))),
        };
        if excerpt.binary {
            return Ok(ToolOutput::error(format!(
                "`{shown}` looks like a binary file and cannot be shown as text."
            )));
        }
        let total = excerpt.total_lines;
        if total == 0 {
            return Ok(ToolOutput::ok(format!("(`{shown}` is empty)")));
        }
        if offset > total {
            return Ok(ToolOutput::error(format!(
                "`{shown}` has {total} lines; offset {offset} is past the end."
            )));
        }
        let last = offset + excerpt.lines.len() - 1;
        let mut content = excerpt
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| number_line(offset + i, &truncate_line(l, MAX_LINE_CHARS)))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = offset > 1 || last < total;
        if truncated {
            content.push_str(&format!(
                "\n\n(Showing lines {offset}-{last} of {total} total lines."
            ));
            if last < total {
                content.push_str(&format!(" Use offset={} to read more.", last + 1));
            }
            content.push(')');
        }
        let mut out = ToolOutput::ok(content);
        out.metadata = json!({
            "path": shown,
            "total_lines": total,
            "first_line": offset,
            "last_line": last,
            "truncated": truncated,
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use vibe_core::Permissions;

    async fn read(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        ReadFileTool.call(ctx, input).await.unwrap()
    }

    #[tokio::test]
    async fn reads_numbered_lines() {
        let (dir, ctx) = workspace();
        write(&dir, "src/a.txt", "alpha\nbeta\r\ngamma");
        let out = read(&ctx, json!({"path": "src/a.txt"})).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "     1\talpha\n     2\tbeta\n     3\tgamma");
        assert_eq!(out.metadata["total_lines"], 3);
    }

    #[tokio::test]
    async fn offset_and_limit_page_through_the_file() {
        let (dir, ctx) = workspace();
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        write(&dir, "f.txt", &body);
        let out = read(&ctx, json!({"path": "f.txt", "offset": 4, "limit": 3})).await;
        assert!(
            out.content
                .starts_with("     4\tline4\n     5\tline5\n     6\tline6")
        );
        assert!(
            out.content
                .contains("Showing lines 4-6 of 10 total lines. Use offset=7")
        );
        let past = read(&ctx, json!({"path": "f.txt", "offset": 50})).await;
        assert!(past.is_error);
    }

    #[tokio::test]
    async fn default_limit_is_2000_lines() {
        let (dir, ctx) = workspace();
        let body: String = (1..=2500).map(|i| format!("{i}\n")).collect();
        write(&dir, "big.txt", &body);
        let out = read(&ctx, json!({"path": "big.txt"})).await;
        assert!(out.content.contains("  2000\t2000"));
        assert!(!out.content.contains("  2001\t2001"));
        assert!(out.content.contains("of 2500 total lines"));
        assert_eq!(out.metadata["truncated"], true);
    }

    #[tokio::test]
    async fn long_lines_are_truncated() {
        let (dir, ctx) = workspace();
        write(&dir, "wide.txt", &"x".repeat(5000));
        let out = read(&ctx, json!({"path": "wide.txt"})).await;
        assert!(
            out.content
                .contains("line truncated, 5000 characters in total")
        );
    }

    #[tokio::test]
    async fn binary_missing_dir_and_empty() {
        let (dir, ctx) = workspace();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 159, 146, 150, 0, 1]).unwrap();
        write(&dir, "empty.txt", "");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(
            read(&ctx, json!({"path": "bin.dat"}))
                .await
                .content
                .contains("binary")
        );
        assert!(read(&ctx, json!({"path": "nope.txt"})).await.is_error);
        assert!(
            read(&ctx, json!({"path": "sub"}))
                .await
                .content
                .contains("list_dir")
        );
        let empty = read(&ctx, json!({"path": "empty.txt"})).await;
        assert!(!empty.is_error && empty.content.contains("empty"));
    }

    #[tokio::test]
    async fn invalid_input_and_permissions() {
        let (dir, ctx) = workspace();
        write(&dir, "a.txt", "a");
        assert!(read(&ctx, json!({"file": "a.txt"})).await.is_error);
        let mut perms = Permissions::local();
        perms.read = false;
        let ctx = ctx.with_permissions(perms);
        let out = read(&ctx, json!({"path": "a.txt"})).await;
        assert!(out.is_error && out.content.contains("Permission denied"));
    }

    #[tokio::test]
    async fn path_escapes_are_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        let abs = outside.path().join("secret");
        for p in ["../secret", "/etc/passwd", abs.to_str().unwrap()] {
            let out = read(&ctx, json!({"path": p})).await;
            assert!(out.is_error && out.content.contains("Access denied"), "{p}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_outside_is_denied() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        let out = read(&ctx, json!({"path": "link"})).await;
        assert!(out.is_error && out.content.contains("Access denied"));
    }
}
