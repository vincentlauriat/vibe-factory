//! File-based persistence of tasks and their artefacts under
//! `<project>/.vibe/tasks/`.
//!
//! ```text
//! .vibe/tasks/
//! ├── index.json                 TaskId -> { dir, number }, next number
//! └── 001-add-oauth-login/
//!     ├── task.json
//!     ├── spec.json   spec.md
//!     ├── plan.json   plan.md
//!     ├── qa_report_1.json   qa_report_1.md   (one pair per QA round)
//!     ├── progress.md                         (append-only, timestamped)
//!     ├── memory/gotchas.md  memory/patterns.md
//!     ├── events.jsonl                        (every event of every run)
//!     └── run.json                            (state of the last run)
//! ```
//!
//! Every JSON and markdown file is written atomically (unique temporary file
//! then rename). Appends (`progress.md`, memory files, `events.jsonl`) and
//! index updates are serialised by an in-process lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use vibe_core::config::VIBE_DIR;
use vibe_core::{
    Envelope, Error, EventSink, Plan, QaReport, Result, RunId, Spec, SubtaskStatus, Task, TaskId,
    TaskStore,
};

use crate::state::RunState;

/// Directory of the task store inside [`VIBE_DIR`].
pub const TASKS_DIR: &str = "tasks";
/// Name of the index file.
pub const INDEX_FILE: &str = "index.json";
/// Name of the run state file inside a task directory.
pub const RUN_FILE: &str = "run.json";
/// Name of the event log inside a task directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// Name of the progress notes inside a task directory.
pub const PROGRESS_FILE: &str = "progress.md";

/// A task-scoped memory file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFile {
    /// Pitfalls (`memory/gotchas.md`).
    Gotchas,
    /// Conventions and patterns (`memory/patterns.md`).
    Patterns,
}

impl MemoryFile {
    /// File name inside the `memory/` directory.
    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            MemoryFile::Gotchas => "gotchas.md",
            MemoryFile::Patterns => "patterns.md",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            MemoryFile::Gotchas => "Gotchas",
            MemoryFile::Patterns => "Patterns",
        }
    }
}

/// Everything the pipeline persists: the core [`TaskStore`] plus run state,
/// task memory and the per-run event log.
///
/// [`FileTaskStore`] implements it. An `Arc<dyn PipelineStore>` coerces to
/// `Arc<dyn TaskStore>` where only the core trait is needed.
#[async_trait::async_trait]
pub trait PipelineStore: TaskStore {
    /// Persist the state of a run (`run.json`).
    async fn save_run_state(&self, state: &RunState) -> Result<()>;
    /// Load the state of the last run of a task, if any.
    async fn load_run_state(&self, id: TaskId) -> Result<Option<RunState>>;
    /// Append a note to a task memory file.
    async fn append_memory(&self, id: TaskId, file: MemoryFile, note: &str) -> Result<()>;
    /// Every memory note of a task, as markdown (empty when none).
    async fn load_memory(&self, id: TaskId) -> Result<String>;
    /// A sink recording the events of `run` for the task, if the store keeps
    /// an event log.
    async fn event_sink(&self, id: TaskId, run: RunId) -> Result<Option<Arc<dyn EventSink>>>;
}

/// Shared handle to a pipeline store.
pub type SharedPipelineStore = Arc<dyn PipelineStore>;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Index {
    #[serde(default)]
    next_number: u32,
    #[serde(default)]
    tasks: BTreeMap<TaskId, IndexEntry>,
}

/// Location of one task in the store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    /// Directory name (`NNN-slug`).
    pub dir: String,
    /// Sequence number, stable for the life of the task.
    pub number: u32,
}

/// [`PipelineStore`] writing JSON and markdown files under
/// `<project>/.vibe/tasks/` (see the module documentation for the layout).
#[derive(Debug)]
pub struct FileTaskStore {
    root: PathBuf,
    index_lock: Mutex<()>,
    append_lock: Mutex<()>,
}

impl FileTaskStore {
    /// Open (creating it if needed) the store of the project at
    /// `project_root`.
    pub fn open(project_root: impl AsRef<Path>) -> Result<Self> {
        let root = project_root.as_ref().join(VIBE_DIR).join(TASKS_DIR);
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            index_lock: Mutex::new(()),
            append_lock: Mutex::new(()),
        })
    }

    /// Directory holding every task (`<project>/.vibe/tasks`).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn read_index(&self) -> Result<Index> {
        let path = self.root.join(INDEX_FILE);
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::storage(format!("corrupt task index {}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(e.into()),
        }
    }

    async fn write_index(&self, index: &Index) -> Result<()> {
        write_json(&self.root.join(INDEX_FILE), index).await
    }

    /// Index entry of a task, if it exists.
    pub async fn entry(&self, id: TaskId) -> Result<Option<IndexEntry>> {
        Ok(self.read_index().await?.tasks.get(&id).cloned())
    }

    /// Directory of an existing task.
    pub async fn task_dir(&self, id: TaskId) -> Result<PathBuf> {
        self.entry(id)
            .await?
            .map(|e| self.root.join(e.dir))
            .ok_or_else(|| Error::storage(format!("unknown task {id}")))
    }

    /// Task with sequence number `number`, if any.
    pub async fn find_by_number(&self, number: u32) -> Result<Option<Task>> {
        let index = self.read_index().await?;
        match index.tasks.iter().find(|(_, e)| e.number == number) {
            Some((id, _)) => Ok(Some(self.load_task(*id).await?)),
            None => Ok(None),
        }
    }

    /// Find a task from a user-supplied reference: a sequence number
    /// (`3`, `003`), a directory name (`003-add-login`), or a prefix of its
    /// id. Returns `Ok(None)` when nothing matches and an error when an id
    /// prefix is ambiguous.
    pub async fn find_by_prefix(&self, reference: &str) -> Result<Option<Task>> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Ok(None);
        }
        let index = self.read_index().await?;
        if reference.len() <= 6
            && let Ok(n) = reference.parse::<u32>()
            && let Some((id, _)) = index.tasks.iter().find(|(_, e)| e.number == n)
        {
            return Ok(Some(self.load_task(*id).await?));
        }
        if let Some((id, _)) = index.tasks.iter().find(|(_, e)| e.dir == reference) {
            return Ok(Some(self.load_task(*id).await?));
        }
        let lower = reference.to_ascii_lowercase();
        let matches: Vec<TaskId> = index
            .tasks
            .keys()
            .filter(|id| id.to_string().starts_with(&lower))
            .copied()
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [id] => Ok(Some(self.load_task(*id).await?)),
            many => Err(Error::storage(format!(
                "task reference `{reference}` is ambiguous ({} tasks match)",
                many.len()
            ))),
        }
    }

    /// Directory of a task, creating the index entry and the directory when
    /// the task is new.
    async fn ensure_dir(&self, task: &Task) -> Result<PathBuf> {
        let _guard = self.index_lock.lock().await;
        let mut index = self.read_index().await?;
        let dir = match index.tasks.get(&task.id) {
            Some(e) => e.dir.clone(),
            None => {
                let used = index.tasks.values().map(|e| e.number).max().unwrap_or(0);
                let number = index.next_number.max(used) + 1;
                index.next_number = number;
                let dir = format!("{number:03}-{}", task.slug());
                index.tasks.insert(
                    task.id,
                    IndexEntry {
                        dir: dir.clone(),
                        number,
                    },
                );
                self.write_index(&index).await?;
                dir
            }
        };
        let path = self.root.join(dir);
        tokio::fs::create_dir_all(&path).await?;
        Ok(path)
    }

    async fn append(&self, path: &Path, text: &str) -> Result<()> {
        let _guard = self.append_lock.lock().await;
        append_file(path, text).await
    }
}

/// Write `bytes` to `path` atomically: a uniquely named temporary file in the
/// same directory, then a rename.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e.into());
    }
    Ok(())
}

async fn write_json<T: serde::Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes).await
}

async fn read_json_opt<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::storage(format!("cannot parse {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn append_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(text.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

async fn read_to_string_opt(path: &Path) -> Result<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

#[async_trait::async_trait]
impl TaskStore for FileTaskStore {
    async fn save_task(&self, task: &Task) -> Result<()> {
        let dir = self.ensure_dir(task).await?;
        write_json(&dir.join("task.json"), task).await
    }

    async fn load_task(&self, id: TaskId) -> Result<Task> {
        let dir = self.task_dir(id).await?;
        read_json_opt(&dir.join("task.json"))
            .await?
            .ok_or_else(|| Error::storage(format!("task {id} has no task.json")))
    }

    async fn list_tasks(&self) -> Result<Vec<Task>> {
        let index = self.read_index().await?;
        let mut out = Vec::with_capacity(index.tasks.len());
        for (id, entry) in &index.tasks {
            if let Some(task) =
                read_json_opt::<Task>(&self.root.join(&entry.dir).join("task.json")).await?
            {
                out.push((entry.number, task));
            } else {
                tracing::warn!(task = %id, "indexed task has no task.json");
            }
        }
        out.sort_by(|a, b| {
            b.1.created_at
                .cmp(&a.1.created_at)
                .then_with(|| b.0.cmp(&a.0))
        });
        Ok(out.into_iter().map(|(_, t)| t).collect())
    }

    async fn delete_task(&self, id: TaskId) -> Result<()> {
        let _guard = self.index_lock.lock().await;
        let mut index = self.read_index().await?;
        let Some(entry) = index.tasks.remove(&id) else {
            return Err(Error::storage(format!("unknown task {id}")));
        };
        self.write_index(&index).await?;
        match tokio::fs::remove_dir_all(self.root.join(entry.dir)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save_spec(&self, spec: &Spec) -> Result<()> {
        let dir = self.task_dir(spec.task_id).await?;
        write_json(&dir.join("spec.json"), spec).await?;
        write_atomic(&dir.join("spec.md"), spec.to_markdown().as_bytes()).await
    }

    async fn load_spec(&self, id: TaskId) -> Result<Option<Spec>> {
        read_json_opt(&self.task_dir(id).await?.join("spec.json")).await
    }

    async fn save_plan(&self, plan: &Plan) -> Result<()> {
        let dir = self.task_dir(plan.task_id).await?;
        write_json(&dir.join("plan.json"), plan).await?;
        write_atomic(&dir.join("plan.md"), plan_to_markdown(plan).as_bytes()).await
    }

    async fn load_plan(&self, id: TaskId) -> Result<Option<Plan>> {
        read_json_opt(&self.task_dir(id).await?.join("plan.json")).await
    }

    async fn save_qa_report(&self, report: &QaReport) -> Result<()> {
        let dir = self.task_dir(report.task_id).await?;
        let stem = format!("qa_report_{}", report.round);
        write_json(&dir.join(format!("{stem}.json")), report).await?;
        write_atomic(
            &dir.join(format!("{stem}.md")),
            report.to_markdown().as_bytes(),
        )
        .await
    }

    async fn load_qa_reports(&self, id: TaskId) -> Result<Vec<QaReport>> {
        let dir = self.task_dir(id).await?;
        let mut reports = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_report = name
                .strip_prefix("qa_report_")
                .and_then(|r| r.strip_suffix(".json"))
                .is_some_and(|n| n.parse::<u32>().is_ok());
            if is_report && let Some(r) = read_json_opt::<QaReport>(&entry.path()).await? {
                reports.push(r);
            }
        }
        reports.sort_by_key(|r| r.round);
        Ok(reports)
    }

    async fn append_progress(&self, id: TaskId, note: &str) -> Result<()> {
        let path = self.task_dir(id).await?.join(PROGRESS_FILE);
        let header = if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            ""
        } else {
            "# Progress notes\n\n"
        };
        let stamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        self.append(
            &path,
            &format!("{header}### {stamp}\n\n{}\n\n", note.trim_end()),
        )
        .await
    }

    async fn load_progress(&self, id: TaskId) -> Result<String> {
        read_to_string_opt(&self.task_dir(id).await?.join(PROGRESS_FILE)).await
    }
}

#[async_trait::async_trait]
impl PipelineStore for FileTaskStore {
    async fn save_run_state(&self, state: &RunState) -> Result<()> {
        let dir = self.task_dir(state.task_id).await?;
        write_json(&dir.join(RUN_FILE), state).await
    }

    async fn load_run_state(&self, id: TaskId) -> Result<Option<RunState>> {
        read_json_opt(&self.task_dir(id).await?.join(RUN_FILE)).await
    }

    async fn append_memory(&self, id: TaskId, file: MemoryFile, note: &str) -> Result<()> {
        let note = note.trim();
        if note.is_empty() {
            return Ok(());
        }
        let path = self
            .task_dir(id)
            .await?
            .join("memory")
            .join(file.file_name());
        let stamp = Utc::now().format("%Y-%m-%d");
        let one_line = note.replace('\n', " ");
        self.append(&path, &format!("- [{stamp}] {one_line}\n"))
            .await
    }

    async fn load_memory(&self, id: TaskId) -> Result<String> {
        let dir = self.task_dir(id).await?.join("memory");
        let mut out = String::new();
        for file in [MemoryFile::Patterns, MemoryFile::Gotchas] {
            let text = read_to_string_opt(&dir.join(file.file_name())).await?;
            if !text.trim().is_empty() {
                out.push_str(&format!("## {}\n\n{}\n", file.heading(), text.trim_end()));
            }
        }
        Ok(out)
    }

    async fn event_sink(&self, id: TaskId, run: RunId) -> Result<Option<Arc<dyn EventSink>>> {
        let path = self.task_dir(id).await?.join(EVENTS_FILE);
        Ok(Some(Arc::new(FileEventSink::new(path).for_run(run))))
    }
}

/// [`EventSink`] appending each [`Envelope`] as one JSON line to a file.
///
/// With [`FileEventSink::for_run`], only events of that run are written
/// (events without a run id are ignored). Write failures are logged, never
/// propagated.
#[derive(Debug)]
pub struct FileEventSink {
    path: PathBuf,
    run: Option<RunId>,
    lock: Mutex<()>,
}

impl FileEventSink {
    /// Sink writing every event to `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            run: None,
            lock: Mutex::new(()),
        }
    }

    /// Only record events of `run`.
    #[must_use]
    pub fn for_run(mut self, run: RunId) -> Self {
        self.run = Some(run);
        self
    }

    /// File the events are appended to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait::async_trait]
impl EventSink for FileEventSink {
    async fn on_event(&self, envelope: &Envelope) {
        if let Some(run) = self.run
            && envelope.event.run_id() != Some(run)
        {
            return;
        }
        let mut line = match serde_json::to_string(envelope) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "cannot serialise event");
                return;
            }
        };
        line.push('\n');
        let _guard = self.lock.lock().await;
        if let Err(e) = append_file(&self.path, &line).await {
            tracing::warn!(error = %e, path = %self.path.display(), "cannot append event");
        }
    }
}

fn status_marker(status: SubtaskStatus) -> &'static str {
    match status {
        SubtaskStatus::Pending => "[ ]",
        SubtaskStatus::InProgress => "[~]",
        SubtaskStatus::Done => "[x]",
        SubtaskStatus::Failed => "[!]",
        SubtaskStatus::Skipped => "[-]",
    }
}

fn status_name(status: SubtaskStatus) -> &'static str {
    match status {
        SubtaskStatus::Pending => "pending",
        SubtaskStatus::InProgress => "in progress",
        SubtaskStatus::Done => "done",
        SubtaskStatus::Failed => "failed",
        SubtaskStatus::Skipped => "skipped",
    }
}

/// Render a plan as markdown, with the status of every subtask.
///
/// Markers: `[ ]` pending, `[~]` in progress, `[x]` done, `[!]` failed,
/// `[-]` skipped. Dependencies are shown by title.
#[must_use]
pub fn plan_to_markdown(plan: &Plan) -> String {
    let titles: BTreeMap<_, _> = plan.subtasks().map(|s| (s.id, s.title.as_str())).collect();
    let done = plan
        .subtasks()
        .filter(|s| s.status == SubtaskStatus::Done)
        .count();
    let mut md = format!(
        "# Implementation plan\n\n**Progress:** {done}/{} subtasks done\n\n",
        plan.len()
    );
    if !plan.approach.trim().is_empty() {
        md.push_str("## Approach\n\n");
        md.push_str(plan.approach.trim());
        md.push_str("\n\n");
    }
    let mut n = 0;
    for (i, phase) in plan.phases.iter().enumerate() {
        md.push_str(&format!(
            "## Phase {}: {}{}\n\n",
            i + 1,
            phase.name,
            if phase.parallel { " (parallel)" } else { "" }
        ));
        for s in &phase.subtasks {
            n += 1;
            md.push_str(&format!(
                "- {} **{n}. {}** ({}",
                status_marker(s.status),
                s.title,
                status_name(s.status)
            ));
            if s.attempts > 0 {
                md.push_str(&format!(", {} attempt(s)", s.attempts));
            }
            md.push_str(")\n");
            for line in s.description.trim().lines() {
                md.push_str(&format!("  {line}\n"));
            }
            if !s.files.is_empty() {
                let files: Vec<String> = s.files.iter().map(|f| format!("`{f}`")).collect();
                md.push_str(&format!("  - Files: {}\n", files.join(", ")));
            }
            if !s.depends_on.is_empty() {
                let deps: Vec<&str> = s
                    .depends_on
                    .iter()
                    .map(|d| titles.get(d).copied().unwrap_or("?"))
                    .collect();
                md.push_str(&format!("  - Depends on: {}\n", deps.join(", ")));
            }
            for v in &s.verification {
                md.push_str(&format!("  - Verify: {v}\n"));
            }
            if !s.notes.trim().is_empty() {
                md.push_str(&format!(
                    "  - Notes: {}\n",
                    s.notes.trim().replace('\n', " ")
                ));
            }
        }
        md.push('\n');
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use vibe_core::{Event, Phase, PlanPhase, QaVerdict, Subtask};

    fn store() -> (tempfile::TempDir, FileTaskStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = FileTaskStore::open(dir.path()).unwrap();
        (dir, s)
    }

    #[tokio::test]
    async fn numbering_is_sequential_and_stable() {
        let (_d, s) = store();
        let a = Task::new("First task", "a");
        let b = Task::new("Second task", "b");
        s.save_task(&a).await.unwrap();
        s.save_task(&b).await.unwrap();
        s.save_task(&a).await.unwrap();
        assert_eq!(s.entry(a.id).await.unwrap().unwrap().dir, "001-first-task");
        assert_eq!(s.entry(b.id).await.unwrap().unwrap().number, 2);
        assert!(s.task_dir(a.id).await.unwrap().join("task.json").exists());
        // Deleting does not reuse numbers.
        s.delete_task(a.id).await.unwrap();
        let c = Task::new("Third", "c");
        s.save_task(&c).await.unwrap();
        assert_eq!(s.entry(c.id).await.unwrap().unwrap().number, 3);
        assert!(s.load_task(a.id).await.is_err());
    }

    #[tokio::test]
    async fn list_newest_first_with_number_tiebreak() {
        let (_d, s) = store();
        let mut a = Task::new("a", "");
        let mut b = Task::new("b", "");
        let now = Utc::now();
        a.created_at = now;
        b.created_at = now;
        s.save_task(&a).await.unwrap();
        s.save_task(&b).await.unwrap();
        let mut c = Task::new("c", "");
        c.created_at = now - chrono::Duration::seconds(10);
        s.save_task(&c).await.unwrap();
        let titles: Vec<String> = s
            .list_tasks()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, vec!["b", "a", "c"]);
    }

    #[tokio::test]
    async fn find_by_number_and_prefix() {
        let (_d, s) = store();
        let a = Task::new("Alpha", "");
        let b = Task::new("Beta", "");
        s.save_task(&a).await.unwrap();
        s.save_task(&b).await.unwrap();
        assert_eq!(s.find_by_number(2).await.unwrap().unwrap().id, b.id);
        assert!(s.find_by_number(9).await.unwrap().is_none());
        assert_eq!(s.find_by_prefix("1").await.unwrap().unwrap().id, a.id);
        assert_eq!(s.find_by_prefix("002").await.unwrap().unwrap().id, b.id);
        assert_eq!(
            s.find_by_prefix("001-alpha").await.unwrap().unwrap().id,
            a.id
        );
        let prefix = &a.id.to_string()[..8];
        assert_eq!(s.find_by_prefix(prefix).await.unwrap().unwrap().id, a.id);
        assert!(s.find_by_prefix("zzzz").await.unwrap().is_none());
        assert!(s.find_by_prefix("").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn artefacts_roundtrip() {
        let (_d, s) = store();
        let t = Task::new("Art", "");
        s.save_task(&t).await.unwrap();
        let dir = s.task_dir(t.id).await.unwrap();

        let spec = Spec {
            task_id: t.id,
            summary: "sum".into(),
            requirements: vec![],
            context: Default::default(),
            body: String::new(),
        };
        s.save_spec(&spec).await.unwrap();
        assert_eq!(s.load_spec(t.id).await.unwrap(), Some(spec));
        assert!(dir.join("spec.md").exists());

        let mut a = Subtask::new("A", "do a");
        a.status = SubtaskStatus::Done;
        let mut b = Subtask::new("B", "do b");
        b.depends_on.push(a.id);
        let plan = Plan {
            task_id: t.id,
            approach: "approach".into(),
            phases: vec![PlanPhase {
                name: "one".into(),
                parallel: true,
                subtasks: vec![a, b],
            }],
        };
        s.save_plan(&plan).await.unwrap();
        assert_eq!(s.load_plan(t.id).await.unwrap(), Some(plan));
        let md = std::fs::read_to_string(dir.join("plan.md")).unwrap();
        assert!(md.contains("[x] **1. A**"));
        assert!(md.contains("Depends on: A"));
        assert!(md.contains("1/2 subtasks done"));

        for round in [2, 1, 10] {
            s.save_qa_report(&QaReport {
                task_id: t.id,
                round,
                verdict: QaVerdict::ChangesRequested,
                summary: String::new(),
                issues: vec![],
            })
            .await
            .unwrap();
        }
        let rounds: Vec<u32> = s
            .load_qa_reports(t.id)
            .await
            .unwrap()
            .iter()
            .map(|r| r.round)
            .collect();
        assert_eq!(rounds, vec![1, 2, 10]);
        assert!(dir.join("qa_report_10.md").exists());

        assert_eq!(s.load_progress(t.id).await.unwrap(), "");
        s.append_progress(t.id, "first").await.unwrap();
        s.append_progress(t.id, "second").await.unwrap();
        let p = s.load_progress(t.id).await.unwrap();
        assert!(p.starts_with("# Progress notes"));
        assert!(p.find("first").unwrap() < p.find("second").unwrap());

        assert_eq!(s.load_memory(t.id).await.unwrap(), "");
        s.append_memory(t.id, MemoryFile::Gotchas, "needs env\nvar")
            .await
            .unwrap();
        s.append_memory(t.id, MemoryFile::Patterns, "use anyhow")
            .await
            .unwrap();
        let m = s.load_memory(t.id).await.unwrap();
        assert!(m.contains("## Gotchas") && m.contains("needs env var"));
        assert!(m.find("Patterns").unwrap() < m.find("Gotchas").unwrap());

        let state = RunState::new(RunId::new(), t.id, Phase::Build);
        s.save_run_state(&state).await.unwrap();
        assert_eq!(s.load_run_state(t.id).await.unwrap(), Some(state));
        // No temporary files are left behind.
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[tokio::test]
    async fn event_sink_filters_by_run() {
        let (_d, s) = store();
        let t = Task::new("Ev", "");
        s.save_task(&t).await.unwrap();
        let run = RunId::new();
        let sink = s.event_sink(t.id, run).await.unwrap().unwrap();
        for r in [run, RunId::new(), run] {
            sink.on_event(&Envelope {
                at: Utc::now(),
                event: Event::PhaseStarted {
                    run: r,
                    phase: Phase::Plan,
                },
            })
            .await;
        }
        let text =
            std::fs::read_to_string(s.task_dir(t.id).await.unwrap().join(EVENTS_FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
        let first: Envelope = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first.event.run_id(), Some(run));
    }

    #[tokio::test]
    async fn concurrent_saves_get_distinct_numbers() {
        let (_d, s) = store();
        let s = Arc::new(s);
        let tasks: Vec<Task> = (0..8).map(|i| Task::new(format!("t{i}"), "")).collect();
        let futs = tasks.iter().map(|t| {
            let s = Arc::clone(&s);
            let t = t.clone();
            async move { s.save_task(&t).await }
        });
        for r in futures::future::join_all(futs).await {
            r.unwrap();
        }
        let mut numbers = Vec::new();
        for t in &tasks {
            numbers.push(s.entry(t.id).await.unwrap().unwrap().number);
        }
        numbers.sort_unstable();
        assert_eq!(numbers, (1..=8).collect::<Vec<_>>());
    }
}
