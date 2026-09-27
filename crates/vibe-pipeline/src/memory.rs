//! Project memory that outlives tasks: `.vibe/memory.jsonl`.
//!
//! [`FileMemoryStore`] keeps one JSON [`MemoryEntry`] per line. Remembering
//! the same fact twice (same kind, same words ignoring case and spacing) is
//! a no-op. Recall ranks entries by the distinct words (three letters or
//! more, common English words excepted) they share with the query, newest
//! first on ties, and returns only entries sharing at least one word.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use vibe_core::{MemoryEntry, MemoryStore, Result};

/// File of the project memory, inside `.vibe`.
pub const MEMORY_FILE: &str = "memory.jsonl";

/// [`MemoryStore`] backed by a JSON lines file.
#[derive(Debug)]
pub struct FileMemoryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileMemoryStore {
    /// Store of the project at `project_root` (`.vibe/memory.jsonl`).
    #[must_use]
    pub fn for_project(project_root: &Path) -> Self {
        Self::new(
            project_root
                .join(vibe_core::config::VIBE_DIR)
                .join(MEMORY_FILE),
        )
    }

    /// Store in `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    /// Path of the file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn read(&self) -> Result<Vec<MemoryEntry>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => Ok(text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Remove every entry.
    pub async fn clear(&self) -> Result<()> {
        let _guard = self.lock.lock().await;
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn normalised(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Words too common to relate two texts.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "are", "was", "were", "has",
    "have", "its", "but", "all", "any", "can", "you", "our", "their", "then", "than", "when",
    "will", "add", "use", "make", "new", "not",
];

/// A crude stem: common English suffixes removed from longer words, so that
/// `exporter`, `exports` and `exporting` all relate to `export`.
fn stem(word: &str) -> String {
    for suffix in ["ing", "ers", "er", "ed", "es", "s"] {
        if let Some(root) = word.strip_suffix(suffix)
            && root.chars().count() >= 4
        {
            return root.to_string();
        }
    }
    word.to_string()
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

#[async_trait::async_trait]
impl MemoryStore for FileMemoryStore {
    async fn remember(&self, entry: MemoryEntry) -> Result<()> {
        let content = entry.content.trim();
        if content.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().await;
        let key = normalised(content);
        if self
            .read()
            .await?
            .iter()
            .any(|e| e.kind == entry.kind && normalised(&e.content) == key)
        {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let wanted = words(query);
        let mut scored: Vec<(usize, MemoryEntry)> = self
            .read()
            .await?
            .into_iter()
            .map(|e| {
                let mut text = e.content.clone();
                for extra in e.files.iter().chain(&e.tags) {
                    text.push(' ');
                    text.push_str(extra);
                }
                (words(&text).intersection(&wanted).count(), e)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.recorded_at.cmp(&a.1.recorded_at))
        });
        Ok(scored.into_iter().take(limit).map(|(_, e)| e).collect())
    }

    async fn all(&self) -> Result<Vec<MemoryEntry>> {
        let mut entries = self.read().await?;
        entries.reverse();
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::MemoryKind;

    #[tokio::test]
    async fn remembers_once_and_recalls_by_shared_words() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileMemoryStore::for_project(dir.path());
        assert!(store.all().await.unwrap().is_empty());
        store
            .remember(MemoryEntry::new(
                MemoryKind::Gotcha,
                "Integration tests need the DATABASE_URL variable",
            ))
            .await
            .unwrap();
        store
            .remember(MemoryEntry::new(
                MemoryKind::Gotcha,
                "integration  tests need the database_url   variable",
            ))
            .await
            .unwrap();
        store
            .remember(MemoryEntry::new(
                MemoryKind::Pattern,
                "Errors use anyhow in the CLI",
            ))
            .await
            .unwrap();
        store
            .remember(MemoryEntry::new(MemoryKind::Pattern, "   "))
            .await
            .unwrap();
        assert_eq!(
            store.all().await.unwrap().len(),
            2,
            "duplicates and blanks skipped"
        );

        let hits = store
            .recall("Add integration tests for the export", 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("DATABASE_URL"));
        assert!(store.recall("zzz qq", 5).await.unwrap().is_empty());
        let reopened = FileMemoryStore::for_project(dir.path());
        assert_eq!(
            reopened.all().await.unwrap()[0].content,
            "Errors use anyhow in the CLI"
        );
        reopened.clear().await.unwrap();
        assert!(reopened.all().await.unwrap().is_empty());
    }

    #[test]
    fn stems() {
        assert_eq!(stem("exporter"), "export");
        assert_eq!(stem("exports"), "export");
        assert_eq!(stem("exporting"), "export");
        assert_eq!(stem("tests"), "test");
        assert_eq!(stem("uses"), "uses");
    }
}
