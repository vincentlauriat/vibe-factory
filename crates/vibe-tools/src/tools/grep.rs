//! `grep`: regular-expression search across text files.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::json;
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

use super::common::{
    MAX_OUTPUT_CHARS, display_path, parse_input, permission_denied, resolve_read, try_output,
    walker, workspace_root,
};
use crate::security::output::{BINARY_SNIFF_BYTES, is_probably_binary};
use crate::text::truncate_line;

/// Files larger than this are skipped.
pub const MAX_GREP_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Characters shown per matching line.
pub const MAX_GREP_LINE_CHARS: usize = 500;
/// Largest accepted `context`.
pub const MAX_GREP_CONTEXT: u32 = 10;

/// Searches file contents with a regular expression.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrepTool;

/// What `grep` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Matching lines as `path:line:text`, with optional context.
    Content,
    /// Paths of matching files only.
    #[default]
    Files,
    /// Number of matching lines per file.
    Count,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    output_mode: OutputMode,
    #[serde(default)]
    context: Option<u32>,
    #[serde(default)]
    case_insensitive: bool,
}

struct Search {
    regex: Regex,
    mode: OutputMode,
    context: usize,
    filter: Option<(GlobMatcher, bool)>,
}

/// Accumulates output until the character budget is spent.
struct Sink {
    out: String,
    full: bool,
}

impl Sink {
    fn push_line(&mut self, line: &str) {
        if self.full {
            return;
        }
        if self.out.len() + line.len() + 1 > MAX_OUTPUT_CHARS {
            self.full = true;
            return;
        }
        self.out.push_str(line);
        self.out.push('\n');
    }
}

impl Search {
    fn accepts(&self, base: &Path, path: &Path) -> bool {
        let Some((glob, whole_path)) = &self.filter else {
            return true;
        };
        if *whole_path {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            glob.is_match(rel)
        } else {
            path.file_name().is_some_and(|n| glob.is_match(n))
        }
    }

    fn read_text(path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > MAX_GREP_FILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
        if is_probably_binary(sniff) {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Search one file; returns the number of matching lines.
    fn search_file(&self, path: &Path, shown: &str, sink: &mut Sink) -> usize {
        let Some(text) = Self::read_text(path) else {
            return 0;
        };
        let lines: Vec<&str> = text.lines().collect();
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| self.regex.is_match(l))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            return 0;
        }
        match self.mode {
            OutputMode::Files => sink.push_line(shown),
            OutputMode::Count => sink.push_line(&format!("{shown}:{}", hits.len())),
            OutputMode::Content => {
                let mut last_printed: Option<usize> = None;
                for &hit in &hits {
                    let start = hit.saturating_sub(self.context);
                    let end = (hit + self.context).min(lines.len() - 1);
                    if let Some(prev) = last_printed
                        && start > prev + 1
                    {
                        sink.push_line("--");
                    }
                    let from = last_printed.map_or(start, |p| start.max(p + 1));
                    for (i, line) in lines.iter().enumerate().take(end + 1).skip(from) {
                        let sep = if hits.binary_search(&i).is_ok() {
                            ':'
                        } else {
                            '-'
                        };
                        let line = truncate_line(line, MAX_GREP_LINE_CHARS);
                        sink.push_line(&format!("{shown}{sep}{}{sep}{line}", i + 1));
                    }
                    last_printed = Some(end.max(last_printed.unwrap_or(0)));
                }
            }
        }
        hits.len()
    }
}

fn collect_files(base: &Path) -> Vec<PathBuf> {
    if base.is_file() {
        return vec![base.to_path_buf()];
    }
    let mut files: Vec<PathBuf> = walker(base, None)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect();
    files.sort();
    files
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression (Rust regex syntax, matched per \
         line). `path` is a file or directory to search (default: the workspace root); \
         `glob` filters files (a pattern without `/` matches file names, e.g. `*.rs`; with \
         `/` it matches paths relative to `path`). `output_mode` is `files` (default: paths \
         of matching files), `content` (matching lines as `path:line:text`, with `context` \
         lines around each match shown as `path-line-text`) or `count` (matching lines per \
         file). Binary files, files over 10 MB and files ignored by `.gitignore` are \
         skipped. Output is capped at 30000 characters."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression to search for."},
                "path": {"type": "string", "description": "File or directory to search, relative to the workspace root (default: the root)."},
                "glob": {"type": "string", "description": "Only search files matching this glob, e.g. `*.rs` or `src/**/*.ts`."},
                "output_mode": {"type": "string", "enum": ["content", "files", "count"], "description": "What to return (default `files`)."},
                "context": {"type": "integer", "minimum": 0, "maximum": MAX_GREP_CONTEXT, "description": "Lines of context around each match in `content` mode (default 0)."},
                "case_insensitive": {"type": "boolean", "description": "Ignore case (default false)."}
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
        let regex = match RegexBuilder::new(&input.pattern)
            .case_insensitive(input.case_insensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "Invalid regular expression: {e}"
                )));
            }
        };
        let filter = match input.glob.as_deref() {
            None | Some("") => None,
            Some(g) => match GlobBuilder::new(g).literal_separator(true).build() {
                Ok(glob) => Some((glob.compile_matcher(), g.contains('/'))),
                Err(e) => return Ok(ToolOutput::error(format!("Invalid glob: {e}."))),
            },
        };
        let base = try_output!(resolve_read(ctx, input.path.as_deref().unwrap_or(".")));
        let root = workspace_root(ctx);
        if !base.exists() {
            return Ok(ToolOutput::error(format!(
                "`{}` does not exist.",
                display_path(&root, &base)
            )));
        }
        let search = Search {
            regex,
            mode: input.output_mode,
            context: input.context.unwrap_or(0).min(MAX_GREP_CONTEXT) as usize,
            filter,
        };
        let (content, files, matches, full) = tokio::task::spawn_blocking(move || {
            let mut sink = Sink {
                out: String::new(),
                full: false,
            };
            let mut files = 0usize;
            let mut matches = 0usize;
            let explicit_file = base.is_file();
            for path in collect_files(&base) {
                if !explicit_file && !search.accepts(&base, &path) {
                    continue;
                }
                let shown = display_path(&root, &path);
                let n = search.search_file(&path, &shown, &mut sink);
                if n > 0 {
                    files += 1;
                    matches += n;
                }
            }
            (sink.out, files, matches, sink.full)
        })
        .await
        .map_err(|e| Error::tool(format!("grep task failed: {e}")))?;

        if files == 0 {
            return Ok(ToolOutput::ok(format!(
                "No matches for `{}`.",
                input.pattern
            )));
        }
        let mut content = content.trim_end().to_string();
        if full {
            content.push_str(&format!(
                "\n\n[Output truncated at {MAX_OUTPUT_CHARS} characters; narrow the search with \
                 `path` or `glob`, or use output_mode `files`.]"
            ));
        }
        let mut out = ToolOutput::ok(content);
        out.metadata = json!({"files": files, "matches": matches, "truncated": full});
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use pretty_assertions::assert_eq;

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        GrepTool.call(ctx, input).await.unwrap()
    }

    fn fixture() -> (tempfile::TempDir, ToolContext) {
        let (dir, ctx) = workspace();
        write(
            &dir,
            "src/a.rs",
            "fn main() {\n    let x = 1;\n    todo!();\n}\n",
        );
        write(&dir, "src/b.rs", "// TODO: later\nfn b() {}\n");
        write(&dir, "notes.md", "todo list\n");
        write(&dir, ".gitignore", "ignored/\n");
        write(&dir, "ignored/c.rs", "todo!()\n");
        std::fs::write(dir.path().join("src/bin.dat"), b"todo\0\x01").unwrap();
        (dir, ctx)
    }

    #[tokio::test]
    async fn files_mode_is_default() {
        let (_dir, ctx) = fixture();
        let out = run(&ctx, json!({"pattern": "todo"})).await;
        assert_eq!(out.content, "notes.md\nsrc/a.rs");
        let ci = run(&ctx, json!({"pattern": "todo", "case_insensitive": true})).await;
        assert_eq!(ci.content, "notes.md\nsrc/a.rs\nsrc/b.rs");
    }

    #[tokio::test]
    async fn content_mode_with_context() {
        let (_dir, ctx) = fixture();
        let out = run(
            &ctx,
            json!({"pattern": "todo!", "output_mode": "content", "glob": "*.rs"}),
        )
        .await;
        assert_eq!(out.content, "src/a.rs:3:    todo!();");
        let out = run(
            &ctx,
            json!({"pattern": "todo!", "output_mode": "content", "context": 1, "path": "src/a.rs"}),
        )
        .await;
        assert_eq!(
            out.content,
            "src/a.rs-2-    let x = 1;\nsrc/a.rs:3:    todo!();\nsrc/a.rs-4-}"
        );
    }

    #[tokio::test]
    async fn content_mode_separates_distant_groups() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "hit\na\nb\nc\nd\nhit\n");
        let out = run(&ctx, json!({"pattern": "hit", "output_mode": "content"})).await;
        assert_eq!(out.content, "f.txt:1:hit\n--\nf.txt:6:hit");
    }

    #[tokio::test]
    async fn count_mode_and_path_glob() {
        let (_dir, ctx) = fixture();
        let out = run(
            &ctx,
            json!({"pattern": "fn", "output_mode": "count", "glob": "src/**/*.rs"}),
        )
        .await;
        assert_eq!(out.content, "src/a.rs:1\nsrc/b.rs:1");
    }

    #[tokio::test]
    async fn no_match_and_errors() {
        let (_dir, ctx) = fixture();
        let out = run(&ctx, json!({"pattern": "zzz"})).await;
        assert!(!out.is_error && out.content.starts_with("No matches"));
        assert!(run(&ctx, json!({"pattern": "("})).await.is_error);
        assert!(
            run(&ctx, json!({"pattern": "x", "glob": "[a"}))
                .await
                .is_error
        );
        assert!(
            run(&ctx, json!({"pattern": "x", "output_mode": "lines"}))
                .await
                .is_error
        );
        assert!(
            run(&ctx, json!({"pattern": "x", "path": "../"}))
                .await
                .is_error
        );
        assert!(
            run(&ctx, json!({"pattern": "x", "path": "missing"}))
                .await
                .is_error
        );
    }

    #[tokio::test]
    async fn output_is_capped() {
        let (dir, ctx) = workspace();
        let body = "match this line please\n".repeat(5000);
        write(&dir, "big.txt", &body);
        let out = run(&ctx, json!({"pattern": "match", "output_mode": "content"})).await;
        assert!(out.content.len() < MAX_OUTPUT_CHARS + 300);
        assert!(out.content.contains("Output truncated at 30000 characters"));
    }

    #[tokio::test]
    async fn absolute_path_outside_is_denied() {
        let (_dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("s.txt"), "needle").unwrap();
        let out = run(
            &ctx,
            json!({"pattern": "needle", "path": outside.path().to_str().unwrap()}),
        )
        .await;
        assert!(out.is_error && out.content.contains("Access denied"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_directories_are_not_followed() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("s.txt"), "unique-outside-needle").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("s.txt"), dir.path().join("file-link"))
            .unwrap();
        let out = run(&ctx, json!({"pattern": "unique-outside-needle"})).await;
        assert!(
            !out.is_error && out.content.starts_with("No matches"),
            "{}",
            out.content
        );
        let direct = run(&ctx, json!({"pattern": "x", "path": "escape"})).await;
        assert!(direct.is_error && direct.content.contains("Access denied"));
    }
}
