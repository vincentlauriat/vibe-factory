//! Optional AI-assisted resolution of merge conflicts.
//!
//! The resolver is generic over [`ModelProvider`]: any backend able to
//! produce a completion can be used. For each conflicted file it sends the
//! file content (with its conflict markers) to the model, asks for the fully
//! merged file, strips a surrounding code fence if the model added one, and
//! writes the result back only when it contains no conflict markers. Files
//! the model could not resolve are left untouched for a human.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use vibe_core::provider::SharedProvider;
use vibe_core::{CompletionRequest, Message, ModelProvider, Result};

/// System prompt sent to the model for every conflicted file.
pub const MERGE_SYSTEM_PROMPT: &str = "You are a code merge expert. You receive one file that \
contains git conflict markers (<<<<<<<, =======, >>>>>>>). Produce the correctly merged file \
that preserves the intent of both sides. Return ONLY the merged file content: no explanations, \
no markdown code fences, and no conflict markers.";

/// How [`crate::GitWorktreeProvider`] handles merge conflicts.
#[derive(Clone, Default)]
pub enum MergeStrategy {
    /// Abort the merge and report the conflicting files for human review.
    #[default]
    Manual,
    /// Ask a model to resolve conflicts; files it cannot resolve are
    /// reported for human review and the merge is aborted.
    Assisted {
        /// Backend used to resolve conflicts.
        provider: SharedProvider,
        /// Provider-specific model identifier.
        model: String,
    },
}

impl fmt::Debug for MergeStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => f.write_str("Manual"),
            Self::Assisted { provider, model } => f
                .debug_struct("Assisted")
                .field("provider", &provider.info().name)
                .field("model", model)
                .finish(),
        }
    }
}

/// Outcome of trying to resolve one conflicted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Path of the file, relative to the workspace root.
    pub file: String,
    /// Whether the file was rewritten without conflict markers.
    pub resolved: bool,
    /// Why the file was not resolved, when it was not.
    pub note: Option<String>,
}

impl Resolution {
    fn resolved(file: &str) -> Self {
        Self {
            file: file.to_string(),
            resolved: true,
            note: None,
        }
    }

    fn unresolved(file: &str, note: impl Into<String>) -> Self {
        Self {
            file: file.to_string(),
            resolved: false,
            note: Some(note.into()),
        }
    }
}

/// Try to resolve the conflict markers of each file in `files` (relative to
/// `workspace_root`) with `provider`.
///
/// A provider failure on one file is recorded as an unresolved
/// [`Resolution`] and does not stop the others. Only I/O failures writing a
/// resolved file are returned as errors.
pub async fn resolve_conflicts(
    provider: &dyn ModelProvider,
    model: &str,
    workspace_root: &Path,
    files: &[String],
) -> Result<Vec<Resolution>> {
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        out.push(resolve_one(provider, model, workspace_root, file).await?);
    }
    Ok(out)
}

async fn resolve_one(
    provider: &dyn ModelProvider,
    model: &str,
    workspace_root: &Path,
    file: &str,
) -> Result<Resolution> {
    let Some(path) = contained_path(workspace_root, file) else {
        return Ok(Resolution::unresolved(
            file,
            "path escapes the workspace root",
        ));
    };
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(Resolution::unresolved(
                file,
                format!("cannot read file: {e}"),
            ));
        }
    };
    if !has_conflict_markers(&content) {
        return Ok(Resolution::resolved(file));
    }

    let mut request = CompletionRequest::new(
        model,
        vec![Message::user(format!(
            "File: {file}\n\nResolve every conflict in this file and return the complete merged \
             file.\n\n{content}"
        ))],
    );
    request.system = MERGE_SYSTEM_PROMPT.to_string();
    request.temperature = Some(0.0);
    request.max_tokens = u32::try_from(content.len() / 3 + 1024)
        .unwrap_or(u32::MAX)
        .clamp(4096, 64_000);

    tracing::debug!(%file, %model, "asking model to resolve conflicts");
    let response = match provider.complete(request).await {
        Ok(r) => r,
        Err(e) => return Ok(Resolution::unresolved(file, format!("model error: {e}"))),
    };
    let mut merged = strip_code_fences(&response.message.text());
    if merged.trim().is_empty() {
        return Ok(Resolution::unresolved(file, "model returned an empty file"));
    }
    if has_conflict_markers(&merged) {
        return Ok(Resolution::unresolved(
            file,
            "model output still contains conflict markers",
        ));
    }
    if content.ends_with('\n') && !merged.ends_with('\n') {
        merged.push('\n');
    }
    tokio::fs::write(&path, merged).await?;
    Ok(Resolution::resolved(file))
}

/// Join `file` to `root`, refusing absolute paths and `..` components.
fn contained_path(root: &Path, file: &str) -> Option<PathBuf> {
    let rel = Path::new(file);
    let safe = rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    (safe && !file.is_empty()).then(|| root.join(rel))
}

/// Whether `text` contains git conflict markers at the start of a line.
#[must_use]
pub fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_end_matches('\r');
        line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line == "======="
    })
}

/// Remove a markdown code fence wrapping the whole answer, if present.
#[must_use]
pub fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return text.to_string();
    }
    let Some((_, body)) = trimmed.split_once('\n') else {
        return String::new();
    };
    let body = body.trim_end();
    let body = body.strip_suffix("```").unwrap_or(body);
    let mut out = body.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use vibe_core::provider::ProviderInfo;
    use vibe_core::{CompletionResponse, Error, StopReason, Usage};

    struct MockProvider {
        answer: std::result::Result<String, String>,
        seen: Mutex<Vec<CompletionRequest>>,
    }

    impl MockProvider {
        fn new(answer: std::result::Result<&str, &str>) -> Self {
            Self {
                answer: answer.map(str::to_string).map_err(str::to_string),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for MockProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "mock".into(),
                supports_tools: false,
                supports_thinking: false,
                default_model: "mock-1".into(),
            }
        }

        async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
            self.seen.lock().unwrap().push(request);
            match &self.answer {
                Ok(text) => Ok(CompletionResponse {
                    message: Message::assistant(text.clone()),
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    model: "mock-1".into(),
                }),
                Err(e) => Err(Error::other(e.clone())),
            }
        }
    }

    const CONFLICTED: &str =
        "fn a() {}\n<<<<<<< HEAD\nlet x = 1;\n=======\nlet x = 2;\n>>>>>>> vibe/t\n";

    #[test]
    fn detects_markers() {
        assert!(has_conflict_markers(CONFLICTED));
        assert!(!has_conflict_markers("a == b\n// ======= not alone\n"));
    }

    #[test]
    fn strips_fences() {
        assert_eq!(
            strip_code_fences("```rust\nlet x = 3;\n```"),
            "let x = 3;\n"
        );
        assert_eq!(strip_code_fences("let x = 3;\n"), "let x = 3;\n");
        assert_eq!(strip_code_fences("  ```\na\nb\n```  \n"), "a\nb\n");
    }

    #[test]
    fn rejects_escaping_paths() {
        let root = Path::new("root");
        assert!(contained_path(root, "src/lib.rs").is_some());
        assert!(contained_path(root, "../etc/passwd").is_none());
        assert!(contained_path(root, "").is_none());
    }

    #[tokio::test]
    async fn resolves_file_and_writes_it_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), CONFLICTED).unwrap();
        let provider = MockProvider::new(Ok("```rust\nfn a() {}\nlet x = 3;\n```"));
        let res = resolve_conflicts(&provider, "mock-1", dir.path(), &["a.rs".to_string()])
            .await
            .unwrap();
        assert_eq!(res, vec![Resolution::resolved("a.rs")]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() {}\nlet x = 3;\n"
        );
        let seen = provider.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].model, "mock-1");
        assert!(seen[0].system.contains("code merge expert"));
        assert!(seen[0].messages[0].text().contains("<<<<<<< HEAD"));
    }

    #[tokio::test]
    async fn keeps_file_when_answer_still_conflicted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), CONFLICTED).unwrap();
        let provider = MockProvider::new(Ok(CONFLICTED));
        let res = resolve_conflicts(&provider, "m", dir.path(), &["a.rs".to_string()])
            .await
            .unwrap();
        assert!(!res[0].resolved);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            CONFLICTED
        );
    }

    #[tokio::test]
    async fn provider_errors_and_missing_files_are_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), CONFLICTED).unwrap();
        std::fs::write(dir.path().join("clean.rs"), "ok\n").unwrap();
        let provider = MockProvider::new(Err("offline"));
        let files = ["a.rs", "missing.rs", "clean.rs"].map(String::from);
        let res = resolve_conflicts(&provider, "m", dir.path(), &files)
            .await
            .unwrap();
        assert!(!res[0].resolved);
        assert!(res[0].note.as_deref().unwrap().contains("offline"));
        assert!(!res[1].resolved);
        assert!(res[2].resolved, "files without markers need no model call");
        assert_eq!(provider.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn strategy_debug_does_not_require_provider_debug() {
        let s = MergeStrategy::Assisted {
            provider: std::sync::Arc::new(MockProvider::new(Ok(""))),
            model: "m".into(),
        };
        assert!(format!("{s:?}").contains("mock"));
        assert_eq!(format!("{:?}", MergeStrategy::default()), "Manual");
    }
}
