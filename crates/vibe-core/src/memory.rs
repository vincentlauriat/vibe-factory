//! Long-lived memory: knowledge that survives a single agent session.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::ids::TaskId;

/// Category of a memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A pitfall discovered the hard way.
    Gotcha,
    /// A convention or pattern used in the codebase.
    Pattern,
    /// A design decision and its rationale.
    Decision,
    /// Where something lives in the codebase.
    Discovery,
    /// An approach that did not work.
    DeadEnd,
    /// Anything else.
    Other(String),
}

/// One remembered fact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryEntry {
    /// Category.
    pub kind: MemoryKind,
    /// The fact itself.
    pub content: String,
    /// Related file paths.
    #[serde(default)]
    pub files: Vec<String>,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Task that produced the entry, if any.
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// When it was recorded.
    pub recorded_at: DateTime<Utc>,
}

impl MemoryEntry {
    /// New entry recorded now.
    pub fn new(kind: MemoryKind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            files: Vec::new(),
            tags: Vec::new(),
            task_id: None,
            recorded_at: Utc::now(),
        }
    }
}

/// Persistent store of [`MemoryEntry`] values.
///
/// The default implementation is a plain in-memory list. Plugins can provide
/// vector search, a knowledge graph, or a shared team memory.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Record an entry.
    async fn remember(&self, entry: MemoryEntry) -> Result<()>;

    /// Retrieve entries relevant to `query` (implementation-defined ranking),
    /// most relevant first, at most `limit`.
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;

    /// Every entry, newest first.
    async fn all(&self) -> Result<Vec<MemoryEntry>>;
}

/// Shared handle to a memory store.
pub type SharedMemory = Arc<dyn MemoryStore>;

/// Simple in-process store with substring ranking. Good enough for tests
/// and single runs.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    entries: tokio::sync::RwLock<Vec<MemoryEntry>>,
}

impl InMemoryStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl MemoryStore for InMemoryStore {
    async fn remember(&self, entry: MemoryEntry) -> Result<()> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| t.len() > 2)
            .collect();
        let entries = self.entries.read().await;
        let mut scored: Vec<(usize, &MemoryEntry)> = entries
            .iter()
            .map(|e| {
                let hay = format!("{} {} {}", e.content, e.files.join(" "), e.tags.join(" "))
                    .to_lowercase();
                let score = terms.iter().filter(|t| hay.contains(t.as_str())).count();
                (score, e)
            })
            .filter(|(s, _)| *s > 0 || terms.is_empty())
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.recorded_at.cmp(&a.1.recorded_at)));
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, e)| e.clone())
            .collect())
    }

    async fn all(&self) -> Result<Vec<MemoryEntry>> {
        let mut v = self.entries.read().await.clone();
        v.reverse();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recall_ranks_by_term_overlap() {
        let store = InMemoryStore::new();
        store
            .remember(MemoryEntry::new(
                MemoryKind::Gotcha,
                "tests need DATABASE_URL",
            ))
            .await
            .unwrap();
        store
            .remember(MemoryEntry::new(
                MemoryKind::Pattern,
                "handlers live in src/api",
            ))
            .await
            .unwrap();
        let hits = store.recall("database tests", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("DATABASE_URL"));
        assert_eq!(store.all().await.unwrap().len(), 2);
    }
}
