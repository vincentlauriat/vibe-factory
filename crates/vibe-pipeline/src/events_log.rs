//! Reading event logs (`events.jsonl`) for interfaces: one log incrementally
//! ([`EventReader`]), every task's log at once ([`FileTaskStore::all_events`])
//! and every log as it grows ([`AllEventsFollower`]).
//!
//! Writers append whole lines ([`crate::FileEventSink`]), but a reader can
//! see a line being written: an incomplete last line is kept until its end
//! arrives. Complete lines that are not envelopes are skipped, as in
//! [`parse_event_log`].

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use vibe_core::{Envelope, Result, TaskId};

use crate::store::{EVENTS_FILE, FileTaskStore, parse_event_log};

/// Reads one event log incrementally, by byte offset.
///
/// Each [`EventReader::read_new`] returns the envelopes appended since the
/// previous call. A log that does not exist yet reads as empty; a log that
/// shrank (truncated) or was replaced by another file is read again from
/// its start.
#[derive(Debug)]
pub struct EventReader {
    path: PathBuf,
    offset: u64,
    pending: Vec<u8>,
    identity: Option<u64>,
}

impl EventReader {
    /// Reader of the log at `path`, from its start.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            pending: Vec::new(),
            identity: None,
        }
    }

    /// Reader of the log at `path` that skips what it already holds: the
    /// first [`EventReader::read_new`] returns only lines appended from now
    /// on (an incomplete last line is read once completed).
    pub async fn at_end(path: impl Into<PathBuf>) -> Result<Self> {
        let mut reader = Self::new(path);
        match tokio::fs::read(&reader.path).await {
            Ok(bytes) => {
                let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
                reader.offset = bytes.len() as u64;
                reader.pending = bytes[end..].to_vec();
                if let Ok(meta) = tokio::fs::metadata(&reader.path).await {
                    reader.identity = file_identity(&meta);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(reader)
    }

    /// The log being read.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes of the log read so far, including an incomplete last line.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset
    }

    fn restart(&mut self) {
        self.offset = 0;
        self.pending.clear();
        self.identity = None;
    }

    /// Envelopes of the lines completed since the last call, in log order.
    pub async fn read_new(&mut self) -> Result<Vec<Envelope>> {
        let mut file = match tokio::fs::File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.restart();
                return Ok(Vec::new());
            }
            Err(e) => return Err(e.into()),
        };
        let meta = file.metadata().await?;
        let identity = file_identity(&meta);
        // A log truncated and then refilled past the offset in place (same
        // file) is not detected: the store only ever appends to its logs.
        if meta.len() < self.offset || (self.identity.is_some() && identity != self.identity) {
            self.restart();
        }
        self.identity = identity;
        if meta.len() == self.offset {
            return Ok(Vec::new());
        }
        file.seek(std::io::SeekFrom::Start(self.offset)).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        self.offset += bytes.len() as u64;
        self.pending.extend_from_slice(&bytes);
        let Some(end) = self.pending.iter().rposition(|b| *b == b'\n') else {
            return Ok(Vec::new());
        };
        // Only whole lines are decoded, so a character cut by a write in
        // progress is never split.
        let complete: Vec<u8> = self.pending.drain(..=end).collect();
        Ok(parse_event_log(&String::from_utf8_lossy(&complete)))
    }
}

#[cfg(unix)]
fn file_identity(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn file_identity(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

/// An envelope with the task whose log it comes from.
///
/// Serialised flat, the envelope's fields next to the task's:
/// `{"task":"…","number":3,"schema":2,"seq":17,"at":"…","event":{…}}`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaggedEnvelope {
    /// Task id.
    pub task: TaskId,
    /// Task sequence number (`003` in `003-add-login`).
    pub number: u32,
    /// The logged envelope.
    #[serde(flatten)]
    pub envelope: Envelope,
}

impl TaggedEnvelope {
    /// Position of this event in the project-wide order.
    #[must_use]
    pub fn cursor(&self) -> EventCursor {
        EventCursor {
            at: self.envelope.at,
            number: self.number,
            seq: self.envelope.seq.unwrap_or(0),
        }
    }
}

/// Position in the project-wide order of events: by time, then task
/// number, then sequence number. A client that resumes a stream asks for
/// the events after the cursor of the last one it saw, so events of other
/// tasks published at the same instant are not lost.
///
/// Its compact text form, `<nanoseconds since the epoch>-<number>-<seq>`
/// (`1790000000123456789-3-17`), fits an SSE `id`. Events without a
/// sequence number (logs recorded before 0.3) use `0`: two of them in one
/// log at the same instant cannot be told apart.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct EventCursor {
    /// Publication time.
    pub at: DateTime<Utc>,
    /// Task sequence number.
    pub number: u32,
    /// Sequence number in the run (0 when the event has none).
    pub seq: u64,
}

impl EventCursor {
    /// Cursor after every event published at or before `at`.
    #[must_use]
    pub fn since_time(at: DateTime<Utc>) -> Self {
        Self {
            at,
            number: u32::MAX,
            seq: u64::MAX,
        }
    }
}

impl std::fmt::Display for EventCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nanos = self.at.timestamp_nanos_opt().unwrap_or(i64::MAX);
        write!(f, "{nanos}-{}-{}", self.number, self.seq)
    }
}

impl std::str::FromStr for EventCursor {
    type Err = vibe_core::Error;

    fn from_str(s: &str) -> Result<Self> {
        let invalid = || vibe_core::Error::config(format!("invalid event cursor `{s}`"));
        let mut parts = s.rsplitn(3, '-');
        let seq = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(invalid)?;
        let number = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(invalid)?;
        let nanos: i64 = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(invalid)?;
        Ok(Self {
            at: DateTime::from_timestamp_nanos(nanos),
            number,
            seq,
        })
    }
}

/// Order of envelopes from several logs (see [`EventCursor`]).
fn by_cursor(a: &TaggedEnvelope, b: &TaggedEnvelope) -> Ordering {
    a.cursor().cmp(&b.cursor())
}

fn tag(
    task: TaskId,
    number: u32,
    envelopes: Vec<Envelope>,
    after: Option<EventCursor>,
) -> impl Iterator<Item = TaggedEnvelope> {
    envelopes
        .into_iter()
        .map(move |envelope| TaggedEnvelope {
            task,
            number,
            envelope,
        })
        .filter(move |e| after.is_none_or(|a| e.cursor() > a))
}

impl FileTaskStore {
    /// Every logged event of every task, after the cursor `after` when
    /// given ([`EventCursor::since_time`] for a time), in [`EventCursor`]
    /// order (the sort is stable: events of one log that tie keep their log
    /// order).
    pub async fn all_events(&self, after: Option<EventCursor>) -> Result<Vec<TaggedEnvelope>> {
        let mut out = Vec::new();
        for (id, entry) in self.entries().await? {
            let path = self.root().join(&entry.dir).join(EVENTS_FILE);
            let text = match tokio::fs::read_to_string(&path).await {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            out.extend(tag(id, entry.number, parse_event_log(&text), after));
        }
        out.sort_by(by_cursor);
        Ok(out)
    }
}

/// Follows the event logs of every task of a store, including tasks
/// created after it started.
///
/// The first [`AllEventsFollower::poll`] returns what
/// [`FileTaskStore::all_events`] would; each later one returns the events
/// appended since, sorted the same way. Tasks removed from the index are
/// dropped.
#[derive(Debug)]
pub struct AllEventsFollower {
    store: FileTaskStore,
    after: Option<EventCursor>,
    readers: BTreeMap<TaskId, (u32, EventReader)>,
    /// Logs [`AllEventsFollower::from_end`] could not read: they are moved
    /// to their end at the first poll that can read them.
    unpositioned: BTreeSet<TaskId>,
}

/// What one [`AllEventsFollower::poll`] found.
#[derive(Debug, Default)]
pub struct PollResult {
    /// New events, in [`EventCursor`] order.
    pub events: Vec<TaggedEnvelope>,
    /// Logs that could not be read this time, with the error. Their reader
    /// has not moved: the next poll tries them again.
    pub errors: Vec<(TaskId, vibe_core::Error)>,
}

impl AllEventsFollower {
    /// Follow every log of `store`, keeping events after the cursor `after`
    /// when given.
    #[must_use]
    pub fn new(store: &FileTaskStore, after: Option<EventCursor>) -> Self {
        Self {
            store: FileTaskStore::at_root(store.root().to_path_buf()),
            after,
            readers: BTreeMap::new(),
            unpositioned: BTreeSet::new(),
        }
    }

    /// Follow every log of `store` from now on: the first poll returns only
    /// events appended after this call (every event of a task created
    /// later). Only an unreadable index fails: a log that cannot be read
    /// now is reported by each poll until it can be, then followed from
    /// its end at that time.
    pub async fn from_end(store: &FileTaskStore) -> Result<Self> {
        let mut follower = Self::new(store, None);
        for (id, entry) in follower.store.entries().await? {
            let path = follower.store.root().join(&entry.dir).join(EVENTS_FILE);
            let reader = match EventReader::at_end(path.clone()).await {
                Ok(reader) => reader,
                Err(_) => {
                    follower.unpositioned.insert(id);
                    EventReader::new(path)
                }
            };
            follower.readers.insert(id, (entry.number, reader));
        }
        Ok(follower)
    }

    /// Events appended to any log since the last poll. Only an unreadable
    /// index fails the poll; a log that cannot be read is reported in
    /// [`PollResult::errors`] without losing the events of the others.
    pub async fn poll(&mut self) -> Result<PollResult> {
        let entries = self.store.entries().await?;
        self.readers.retain(|id, _| entries.contains_key(id));
        self.unpositioned.retain(|id| entries.contains_key(id));
        for (id, entry) in &entries {
            self.readers.entry(*id).or_insert_with(|| {
                let path = self.store.root().join(&entry.dir).join(EVENTS_FILE);
                (entry.number, EventReader::new(path))
            });
        }
        let mut out = PollResult::default();
        for (id, (number, reader)) in &mut self.readers {
            if self.unpositioned.contains(id) {
                match EventReader::at_end(reader.path().to_path_buf()).await {
                    Ok(at_end) => {
                        *reader = at_end;
                        self.unpositioned.remove(id);
                    }
                    Err(e) => out.errors.push((*id, e)),
                }
                continue;
            }
            match reader.read_new().await {
                Ok(envelopes) => out.events.extend(tag(*id, *number, envelopes, self.after)),
                Err(e) => out.errors.push((*id, e)),
            }
        }
        out.events.sort_by(by_cursor);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::Write;
    use vibe_core::{Event, Phase, RunId, Task, TaskStore};

    fn line(seq: u64, at: DateTime<Utc>, run: RunId) -> String {
        let mut env = Envelope::now(Event::PhaseStarted {
            run,
            phase: Phase::Plan,
        });
        env.seq = Some(seq);
        env.at = at;
        format!("{}\n", serde_json::to_string(&env).unwrap())
    }

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    fn seqs(envs: &[Envelope]) -> Vec<u64> {
        envs.iter().map(|e| e.seq.unwrap()).collect()
    }

    #[tokio::test]
    async fn reads_appended_chunks_and_waits_for_a_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EVENTS_FILE);
        let run = RunId::new();
        let now = Utc::now();
        let mut reader = EventReader::new(&path);
        // Missing file: nothing yet.
        assert!(reader.read_new().await.unwrap().is_empty());

        append(&path, &(line(1, now, run) + &line(2, now, run)));
        assert_eq!(seqs(&reader.read_new().await.unwrap()), vec![1, 2]);
        assert!(reader.read_new().await.unwrap().is_empty());

        // A line cut in the middle, then completed.
        let third = line(3, now, run);
        let (head, rest) = third.split_at(20);
        append(&path, head);
        assert!(reader.read_new().await.unwrap().is_empty());
        append(
            &path,
            &(rest.to_string() + "not json\n" + &line(4, now, run)),
        );
        assert_eq!(seqs(&reader.read_new().await.unwrap()), vec![3, 4]);
        assert_eq!(reader.offset(), std::fs::metadata(&path).unwrap().len());
    }

    #[tokio::test]
    async fn restarts_when_the_log_is_truncated_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EVENTS_FILE);
        let run = RunId::new();
        let now = Utc::now();
        let mut reader = EventReader::new(&path);
        append(&path, &(line(1, now, run) + &line(2, now, run)));
        assert_eq!(reader.read_new().await.unwrap().len(), 2);

        std::fs::write(&path, line(7, now, run)).unwrap();
        assert_eq!(seqs(&reader.read_new().await.unwrap()), vec![7]);

        std::fs::remove_file(&path).unwrap();
        assert!(reader.read_new().await.unwrap().is_empty());
        append(&path, &line(9, now, run));
        assert_eq!(seqs(&reader.read_new().await.unwrap()), vec![9]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restarts_when_the_log_is_replaced_by_a_longer_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EVENTS_FILE);
        let run = RunId::new();
        let now = Utc::now();
        let mut reader = EventReader::new(&path);
        append(&path, &line(1, now, run));
        assert_eq!(reader.read_new().await.unwrap().len(), 1);
        let other = dir.path().join("other.jsonl");
        append(&other, &(line(5, now, run) + &line(6, now, run)));
        std::fs::rename(&other, &path).unwrap();
        assert_eq!(seqs(&reader.read_new().await.unwrap()), vec![5, 6]);
    }

    #[test]
    fn tagged_envelopes_serialise_flat() {
        let env = Envelope::now(Event::PhaseStarted {
            run: RunId::new(),
            phase: Phase::Build,
        });
        let tagged = TaggedEnvelope {
            task: TaskId::new(),
            number: 3,
            envelope: env,
        };
        let value = serde_json::to_value(&tagged).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["at", "event", "number", "schema", "task"]);
        let back: TaggedEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(back, tagged);
    }

    #[tokio::test]
    async fn all_events_merge_every_task_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let a = Task::new("Alpha", "");
        let b = Task::new("Beta", "");
        store.save_task(&a).await.unwrap();
        store.save_task(&b).await.unwrap();
        let log_a = store.task_dir(a.id).await.unwrap().join(EVENTS_FILE);
        let log_b = store.task_dir(b.id).await.unwrap().join(EVENTS_FILE);
        let (ra, rb) = (RunId::new(), RunId::new());
        let t0 = Utc::now() - chrono::Duration::seconds(60);
        let at = |s: i64| t0 + chrono::Duration::seconds(s);
        append(&log_a, &(line(1, at(0), ra) + &line(2, at(20), ra)));
        append(&log_b, &(line(1, at(10), rb) + &line(2, at(20), rb)));

        let order = |events: &[TaggedEnvelope]| -> Vec<(u32, u64)> {
            events
                .iter()
                .map(|e| (e.number, e.envelope.seq.unwrap()))
                .collect()
        };
        let all = store.all_events(None).await.unwrap();
        assert_eq!(order(&all), vec![(1, 1), (2, 1), (1, 2), (2, 2)]);
        assert_eq!(all[1].task, b.id);
        let recent = store
            .all_events(Some(EventCursor::since_time(at(10))))
            .await
            .unwrap();
        assert_eq!(order(&recent), vec![(1, 2), (2, 2)]);

        // The follower starts with the same view, then sees appends and new
        // tasks.
        let mut follower = AllEventsFollower::new(&store, Some(EventCursor::since_time(at(0))));
        assert_eq!(
            order(&follower.poll().await.unwrap().events),
            vec![(2, 1), (1, 2), (2, 2)]
        );
        assert!(follower.poll().await.unwrap().events.is_empty());
        let c = Task::new("Gamma", "");
        store.save_task(&c).await.unwrap();
        let log_c = store.task_dir(c.id).await.unwrap().join(EVENTS_FILE);
        append(&log_c, &line(1, at(30), RunId::new()));
        append(&log_a, &line(3, at(40), ra));
        assert_eq!(
            order(&follower.poll().await.unwrap().events),
            vec![(3, 1), (1, 3)]
        );

        // Events of two tasks at the same instant: resuming after the first
        // one's cursor still delivers the second.
        let tie = store.all_events(None).await.unwrap();
        let first_at_20 = tie.iter().find(|e| e.envelope.at == at(20)).unwrap();
        assert_eq!(first_at_20.number, 1);
        let cursor: EventCursor = first_at_20.cursor().to_string().parse().unwrap();
        assert_eq!(cursor, first_at_20.cursor());
        let resumed = store.all_events(Some(cursor)).await.unwrap();
        assert_eq!(order(&resumed)[0], (2, 2));

        store.delete_task(b.id).await.unwrap();
        follower.poll().await.unwrap();
        assert!(!follower.readers.contains_key(&b.id));
    }

    #[tokio::test]
    async fn an_unreadable_log_does_not_lose_the_events_of_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let mut ids = Vec::new();
        // Fixed ids: the unreadable log is read between the two others.
        for (i, title) in ["a", "b", "c"].iter().enumerate() {
            let mut t = Task::new(*title, "");
            t.id = TaskId::parse(&format!("00000000-0000-4000-8000-00000000000{}", i + 1)).unwrap();
            store.save_task(&t).await.unwrap();
            ids.push(t.id);
        }
        let run = RunId::new();
        let now = Utc::now();
        for id in [ids[0], ids[2]] {
            let log = store.task_dir(id).await.unwrap().join(EVENTS_FILE);
            append(&log, &line(1, now, run));
        }
        let broken = store.task_dir(ids[1]).await.unwrap().join(EVENTS_FILE);
        std::fs::create_dir(&broken).unwrap();

        let mut follower = AllEventsFollower::new(&store, None);
        let first = follower.poll().await.unwrap();
        let tasks: Vec<TaskId> = first.events.iter().map(|e| e.task).collect();
        assert_eq!(tasks, vec![ids[0], ids[2]]);
        assert_eq!(first.errors.len(), 1);
        assert_eq!(first.errors[0].0, ids[1]);

        // Repaired: its events arrive, and nothing is delivered twice.
        std::fs::remove_dir(&broken).unwrap();
        append(&broken, &line(1, now, run));
        let second = follower.poll().await.unwrap();
        assert!(second.errors.is_empty());
        let tasks: Vec<TaskId> = second.events.iter().map(|e| e.task).collect();
        assert_eq!(tasks, vec![ids[1]]);
    }

    #[tokio::test]
    async fn a_follower_from_the_end_reports_an_unreadable_log_and_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let (a, b) = (Task::new("Alpha", ""), Task::new("Beta", ""));
        store.save_task(&a).await.unwrap();
        store.save_task(&b).await.unwrap();
        let log_a = store.task_dir(a.id).await.unwrap().join(EVENTS_FILE);
        let broken = store.task_dir(b.id).await.unwrap().join(EVENTS_FILE);
        std::fs::create_dir(&broken).unwrap();
        let run = RunId::new();
        let now = Utc::now();
        append(&log_a, &line(1, now, run));

        let mut follower = AllEventsFollower::from_end(&store).await.unwrap();
        append(&log_a, &line(2, now, run));
        let first = follower.poll().await.unwrap();
        let seqs: Vec<u64> = first
            .events
            .iter()
            .map(|e| e.envelope.seq.unwrap())
            .collect();
        assert_eq!(seqs, vec![2]);
        assert_eq!(first.errors.len(), 1);
        assert_eq!(first.errors[0].0, b.id);

        // Repaired with a history: only what is appended afterwards comes.
        std::fs::remove_dir(&broken).unwrap();
        append(&broken, &line(1, now, run));
        let second = follower.poll().await.unwrap();
        assert!(second.errors.is_empty());
        assert!(second.events.is_empty());
        append(&broken, &line(2, now, run));
        let third = follower.poll().await.unwrap();
        let got: Vec<(TaskId, u64)> = third
            .events
            .iter()
            .map(|e| (e.task, e.envelope.seq.unwrap()))
            .collect();
        assert_eq!(got, vec![(b.id, 2)]);
    }

    #[tokio::test]
    async fn a_follower_from_the_end_skips_what_is_already_logged() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTaskStore::open(dir.path()).unwrap();
        let a = Task::new("Alpha", "");
        store.save_task(&a).await.unwrap();
        let log = store.task_dir(a.id).await.unwrap().join(EVENTS_FILE);
        let run = RunId::new();
        let now = Utc::now();
        let third = line(3, now, run);
        let (head, rest) = third.split_at(10);
        append(&log, &(line(1, now, run) + &line(2, now, run) + head));

        let mut follower = AllEventsFollower::from_end(&store).await.unwrap();
        assert!(follower.poll().await.unwrap().events.is_empty());
        append(&log, &(rest.to_string() + &line(4, now, run)));
        let b = Task::new("Beta", "");
        store.save_task(&b).await.unwrap();
        let log_b = store.task_dir(b.id).await.unwrap().join(EVENTS_FILE);
        append(&log_b, &line(1, now, RunId::new()));
        let got: Vec<(u32, u64)> = follower
            .poll()
            .await
            .unwrap()
            .events
            .iter()
            .map(|e| (e.number, e.envelope.seq.unwrap()))
            .collect();
        assert_eq!(got, vec![(1, 3), (1, 4), (2, 1)]);
    }
}
