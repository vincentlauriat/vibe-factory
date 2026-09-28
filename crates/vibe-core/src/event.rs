//! Events emitted while the framework works, and the bus that carries them.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::agent::AgentRole;
use crate::ids::{CallId, RunId, SubtaskId, TaskId};
use crate::phase::Phase;
use crate::provider::{StreamDelta, Usage};

/// Something that happened.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Event {
    /// A pipeline run started for a task.
    RunStarted {
        /// Run id.
        run: RunId,
        /// Task id.
        task: TaskId,
    },
    /// A phase started.
    PhaseStarted {
        /// Run id.
        run: RunId,
        /// Phase.
        phase: Phase,
    },
    /// A phase finished.
    PhaseFinished {
        /// Run id.
        run: RunId,
        /// Phase.
        phase: Phase,
        /// Whether it succeeded.
        success: bool,
        /// Short summary.
        summary: String,
    },
    /// An agent session started.
    AgentStarted {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Subtask being worked on, if any.
        subtask: Option<SubtaskId>,
        /// Model id the session uses (empty in logs recorded before 0.5).
        #[serde(default)]
        model: String,
    },
    /// The model produced text.
    AgentText {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Text chunk.
        text: String,
    },
    /// Part of a model answer, while it is being generated. Ephemeral: not
    /// written to the run's event log, since the complete text follows in
    /// [`Event::AgentText`].
    AgentDelta {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Subtask being worked on, if any.
        subtask: Option<SubtaskId>,
        /// The new text.
        delta: StreamDelta,
    },
    /// The model requested a tool.
    ToolCalled {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Tool name.
        tool: String,
        /// Arguments.
        input: serde_json::Value,
        /// Id of the call, repeated by its [`Event::ToolReturned`]. Nil in
        /// logs recorded before 0.5, where calls pair by order.
        #[serde(default)]
        call: CallId,
        /// Subtask being worked on, if any.
        #[serde(default)]
        subtask: Option<SubtaskId>,
    },
    /// A tool returned.
    ToolReturned {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Tool name.
        tool: String,
        /// Whether it failed.
        is_error: bool,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Preview of the output.
        preview: String,
        /// Id of the call (see [`Event::ToolCalled`]).
        #[serde(default)]
        call: CallId,
        /// Subtask being worked on, if any.
        #[serde(default)]
        subtask: Option<SubtaskId>,
        /// Exit code, for tools that run a command.
        #[serde(default)]
        exit_code: Option<i64>,
        /// Whether the command was stopped by its timeout.
        #[serde(default)]
        timed_out: bool,
        /// Length of the complete output, in characters.
        #[serde(default)]
        output_chars: u64,
        /// Complete output, relative to the project root
        /// (`.vibe/tool-output/<task>/<run>/<call>.txt`), when traced.
        #[serde(default)]
        output_file: Option<String>,
    },
    /// An agent session ended.
    AgentFinished {
        /// Run id.
        run: RunId,
        /// Role.
        role: AgentRole,
        /// Steps taken.
        steps: u32,
        /// Tokens used.
        usage: Usage,
        /// Stop reason (serialised [`crate::AgentStop`]).
        stop: String,
    },
    /// A subtask changed state.
    SubtaskUpdated {
        /// Run id.
        run: RunId,
        /// Subtask.
        subtask: SubtaskId,
        /// New status.
        status: crate::plan::SubtaskStatus,
    },
    /// A required validation command finished.
    ValidationFinished {
        /// Run id.
        run: RunId,
        /// The configured command.
        command: String,
        /// True when it checked the combined integration candidate.
        integration: bool,
        /// Whether it passed.
        passed: bool,
        /// Exit code, when the command ran to completion.
        exit_code: Option<i64>,
    },
    /// A finished subtask attempt was integrated into the task workspace, or
    /// conflicted with work integrated since it started.
    SubtaskIntegrated {
        /// Run id.
        run: RunId,
        /// Subtask.
        subtask: SubtaskId,
        /// Resulting commit of the task workspace, if any.
        commit: Option<String>,
        /// Conflicting files; empty when the integration succeeded.
        conflicts: Vec<String>,
    },
    /// The pipeline committed work in a workspace (checkpoints, subtask
    /// and fix commits, subtask integrations).
    Committed {
        /// Run id.
        run: RunId,
        /// Subtask the commit belongs to, if any.
        subtask: Option<SubtaskId>,
        /// Commit id.
        commit: String,
        /// First line of the commit message.
        message: String,
        /// Files changed by the commit, relative to the repository root.
        files: Vec<String>,
    },
    /// The task branch was merged into its base branch.
    Merged {
        /// Run id.
        run: RunId,
        /// Resulting commit on the base branch.
        commit: String,
        /// Merged branch.
        branch: String,
        /// Branch merged into.
        base: String,
    },
    /// The consumption of the run budget changed.
    BudgetUpdated {
        /// Run id.
        run: RunId,
        /// Tokens used by the whole run, resumes included.
        tokens: u64,
        /// Token limit, if any.
        token_limit: Option<u64>,
        /// Active time of the whole run in milliseconds.
        active_ms: u64,
        /// Duration limit in milliseconds, if any.
        duration_limit_ms: Option<u64>,
    },
    /// An artefact of the task was written.
    ArtefactWritten {
        /// Run id.
        run: RunId,
        /// Which artefact.
        artefact: Artefact,
    },
    /// The run stopped to wait for a human decision.
    ApprovalRequested {
        /// Run id.
        run: RunId,
        /// What is to be approved.
        gate: crate::config::ApprovalGate,
    },
    /// A human approved or rejected.
    ApprovalResolved {
        /// Run id.
        run: RunId,
        /// What was decided on.
        gate: crate::config::ApprovalGate,
        /// Whether it was approved.
        approved: bool,
        /// The approver's note or the reason of the rejection.
        comment: String,
    },
    /// A retry is about to happen.
    Retrying {
        /// Run id.
        run: RunId,
        /// What is being retried.
        what: String,
        /// Attempt number (1-based).
        attempt: u32,
        /// Delay before the retry, in milliseconds.
        delay_ms: u64,
    },
    /// The run is waiting for a human.
    Paused {
        /// Run id.
        run: RunId,
        /// Why.
        reason: String,
    },
    /// The run finished.
    RunFinished {
        /// Run id.
        run: RunId,
        /// Whether the task reached a successful terminal state.
        success: bool,
        /// Final task status.
        status: crate::task::TaskStatus,
        /// Tokens used by the whole run, resumes included.
        #[serde(default)]
        usage: Usage,
        /// Active time of the whole run in milliseconds, resumes included.
        #[serde(default)]
        active_ms: u64,
        /// When the run started (the Unix epoch in logs recorded before 0.5).
        #[serde(default)]
        started_at: DateTime<Utc>,
    },
    /// Free-form diagnostic.
    Log {
        /// Run id, if any.
        run: Option<RunId>,
        /// Severity (`info`, `warn`, `error`).
        level: String,
        /// Message.
        message: String,
    },
}

impl Event {
    /// Every value of the `type` tag, in declaration order.
    pub const TYPES: &'static [&'static str] = &[
        "run_started",
        "phase_started",
        "phase_finished",
        "agent_started",
        "agent_text",
        "agent_delta",
        "tool_called",
        "tool_returned",
        "agent_finished",
        "subtask_updated",
        "validation_finished",
        "subtask_integrated",
        "committed",
        "merged",
        "budget_updated",
        "artefact_written",
        "approval_requested",
        "approval_resolved",
        "retrying",
        "paused",
        "run_finished",
        "log",
    ];

    /// The `type` tag of the event.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::RunStarted { .. } => "run_started",
            Event::PhaseStarted { .. } => "phase_started",
            Event::PhaseFinished { .. } => "phase_finished",
            Event::AgentStarted { .. } => "agent_started",
            Event::AgentText { .. } => "agent_text",
            Event::AgentDelta { .. } => "agent_delta",
            Event::ToolCalled { .. } => "tool_called",
            Event::ToolReturned { .. } => "tool_returned",
            Event::AgentFinished { .. } => "agent_finished",
            Event::SubtaskUpdated { .. } => "subtask_updated",
            Event::ValidationFinished { .. } => "validation_finished",
            Event::SubtaskIntegrated { .. } => "subtask_integrated",
            Event::Committed { .. } => "committed",
            Event::Merged { .. } => "merged",
            Event::BudgetUpdated { .. } => "budget_updated",
            Event::ArtefactWritten { .. } => "artefact_written",
            Event::ApprovalRequested { .. } => "approval_requested",
            Event::ApprovalResolved { .. } => "approval_resolved",
            Event::Retrying { .. } => "retrying",
            Event::Paused { .. } => "paused",
            Event::RunFinished { .. } => "run_finished",
            Event::Log { .. } => "log",
        }
    }

    /// Run this event belongs to, if any.
    #[must_use]
    pub fn run_id(&self) -> Option<RunId> {
        match self {
            Event::RunStarted { run, .. }
            | Event::PhaseStarted { run, .. }
            | Event::PhaseFinished { run, .. }
            | Event::AgentStarted { run, .. }
            | Event::AgentText { run, .. }
            | Event::AgentDelta { run, .. }
            | Event::ToolCalled { run, .. }
            | Event::ToolReturned { run, .. }
            | Event::AgentFinished { run, .. }
            | Event::SubtaskUpdated { run, .. }
            | Event::ValidationFinished { run, .. }
            | Event::SubtaskIntegrated { run, .. }
            | Event::Committed { run, .. }
            | Event::Merged { run, .. }
            | Event::BudgetUpdated { run, .. }
            | Event::ArtefactWritten { run, .. }
            | Event::ApprovalRequested { run, .. }
            | Event::ApprovalResolved { run, .. }
            | Event::Retrying { run, .. }
            | Event::Paused { run, .. }
            | Event::RunFinished { run, .. } => Some(*run),
            Event::Log { run, .. } => *run,
        }
    }
}

/// A task artefact, as named by [`Event::ArtefactWritten`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Artefact {
    /// `spec.json` and `spec.md`.
    Spec,
    /// `plan.json` and `plan.md`, as written by the planner.
    Plan,
    /// `qa_report_<round>.json` and `.md`.
    QaReport {
        /// Review round.
        round: u32,
    },
}

impl Event {
    /// Whether the event is only meaningful live (streamed text) and is not
    /// kept in persistent logs.
    #[must_use]
    pub fn is_ephemeral(&self) -> bool {
        matches!(self, Event::AgentDelta { .. })
    }
}

/// Version of the event format written in every [`Envelope`]. Logs written
/// before versioning read as version 1.
pub const EVENT_SCHEMA_VERSION: u32 = 2;

fn legacy_schema() -> u32 {
    1
}

/// An event with its timestamp, schema version and sequence number.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    /// Version of the event format ([`EVENT_SCHEMA_VERSION`]).
    #[serde(default = "legacy_schema")]
    pub schema: u32,
    /// Position of the event in its run, from 1, without gaps and kept across
    /// resumes. `None` for ephemeral events and events without a run: those
    /// are never replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// When the event was published.
    pub at: DateTime<Utc>,
    /// The event.
    pub event: Event,
}

impl Envelope {
    /// Envelope of `event` published now, without a sequence number.
    #[must_use]
    pub fn now(event: Event) -> Self {
        Self {
            schema: EVENT_SCHEMA_VERSION,
            seq: None,
            at: Utc::now(),
            event,
        }
    }
}

/// Something that consumes events (a logger, a UI, a file writer, …).
#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    /// Handle one event. Must not block for long.
    async fn on_event(&self, envelope: &Envelope);
}

/// Broadcasts events to subscribers and registered sinks.
#[derive(Clone)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<Envelope>,
    sinks: Arc<tokio::sync::RwLock<Vec<Arc<dyn EventSink>>>>,
    /// Last sequence number given to each run.
    seqs: Arc<std::sync::Mutex<std::collections::HashMap<RunId, u64>>>,
    /// Held while a numbered event is delivered, so that subscribers and
    /// sinks see sequence numbers in order.
    order: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("receivers", &self.tx.receiver_count())
            .finish()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl EventBus {
    /// Create a bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(capacity);
        Self {
            tx,
            sinks: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            seqs: Arc::default(),
            order: Arc::default(),
        }
    }

    /// Continue the numbering of `run` after `last` (the highest sequence
    /// number already logged), for a resumed run. Never goes backwards.
    pub fn resume_sequence(&self, run: RunId, last: u64) {
        let mut seqs = self.seqs.lock().unwrap_or_else(|e| e.into_inner());
        let current = seqs.entry(run).or_insert(0);
        *current = (*current).max(last);
    }

    fn next_seq(&self, event: &Event) -> Option<u64> {
        if event.is_ephemeral() {
            return None;
        }
        let run = event.run_id()?;
        let mut seqs = self.seqs.lock().unwrap_or_else(|e| e.into_inner());
        let n = seqs.entry(run).or_insert(0);
        *n += 1;
        Some(*n)
    }

    /// Subscribe to a live stream of events.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Envelope> {
        self.tx.subscribe()
    }

    /// Register a sink that is awaited on every publish.
    pub async fn add_sink(&self, sink: Arc<dyn EventSink>) {
        self.sinks.write().await.push(sink);
    }

    /// Publish an event.
    pub async fn publish(&self, event: Event) {
        let numbered = !event.is_ephemeral() && event.run_id().is_some();
        let _order = if numbered {
            Some(self.order.lock().await)
        } else {
            None
        };
        let mut envelope = Envelope::now(event);
        envelope.seq = self.next_seq(&envelope.event);
        // A send error only means nobody is listening.
        let _ = self.tx.send(envelope.clone());
        for sink in self.sinks.read().await.iter() {
            sink.on_event(&envelope).await;
        }
    }

    /// Publish a log event.
    pub async fn log(&self, run: Option<RunId>, level: &str, message: impl Into<String>) {
        self.publish(Event::Log {
            run,
            level: level.to_string(),
            message: message.into(),
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl EventSink for Counter {
        async fn on_event(&self, _e: &Envelope) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn publish_reaches_subscribers_and_sinks() {
        let bus = EventBus::default();
        let counter = Arc::new(Counter(std::sync::atomic::AtomicUsize::new(0)));
        bus.add_sink(counter.clone()).await;
        let mut rx = bus.subscribe();
        let run = RunId::new();
        bus.publish(Event::RunStarted {
            run,
            task: TaskId::new(),
        })
        .await;
        let got = rx.recv().await.unwrap();
        assert_eq!(got.event.run_id(), Some(run));
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn numbering_skips_ephemeral_events_and_resumes() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        let run = RunId::new();
        bus.resume_sequence(run, 41);
        bus.publish(Event::PhaseStarted {
            run,
            phase: crate::Phase::Build,
        })
        .await;
        bus.publish(Event::AgentDelta {
            run,
            role: crate::AgentRole::Coder,
            subtask: None,
            delta: StreamDelta::Text { text: "x".into() },
        })
        .await;
        bus.log(None, "info", "no run").await;
        bus.publish(Event::Paused {
            run,
            reason: "r".into(),
        })
        .await;
        let seqs: Vec<Option<u64>> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![Some(42), None, None, Some(43)]);
        // Never backwards.
        bus.resume_sequence(run, 5);
        bus.publish(Event::Paused {
            run,
            reason: "r".into(),
        })
        .await;
        assert_eq!(rx.recv().await.unwrap().seq, Some(44));
    }

    #[test]
    fn logs_without_version_read_as_version_one() {
        let line = format!(
            r#"{{"at":"2026-09-26T10:00:00Z","event":{{"type":"paused","run":"{}","reason":"x"}}}}"#,
            RunId::new()
        );
        let e: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(e.schema, 1);
        assert_eq!(e.seq, None);
        let json = serde_json::to_string(&Envelope::now(e.event)).unwrap();
        assert!(json.contains("\"schema\":2"));
        assert!(!json.contains("\"seq\""));
    }

    #[test]
    fn type_names_match_serde_tags() {
        let run = RunId::new();
        let sample = Event::BudgetUpdated {
            run,
            tokens: 1,
            token_limit: None,
            active_ms: 0,
            duration_limit_ms: None,
        };
        let json = serde_json::to_value(&sample).unwrap();
        assert_eq!(json["type"], sample.type_name());
        assert!(Event::TYPES.contains(&sample.type_name()));
        let paused = Event::Paused {
            run,
            reason: String::new(),
        };
        assert_eq!(serde_json::to_value(&paused).unwrap()["type"], "paused");
        assert_eq!(paused.type_name(), "paused");
    }

    #[test]
    fn schema_two_logs_without_the_new_fields_still_read() {
        let run = RunId::new();
        let lines = [
            format!(
                r#"{{"schema":2,"seq":1,"at":"2026-09-26T10:00:00Z","event":{{"type":"agent_started","run":"{run}","role":"coder","subtask":null}}}}"#
            ),
            format!(
                r#"{{"schema":2,"seq":2,"at":"2026-09-26T10:00:00Z","event":{{"type":"tool_called","run":"{run}","role":"coder","tool":"bash","input":{{"command":"ls"}}}}}}"#
            ),
            format!(
                r#"{{"schema":2,"seq":3,"at":"2026-09-26T10:00:01Z","event":{{"type":"tool_returned","run":"{run}","role":"coder","tool":"bash","is_error":false,"duration_ms":5,"preview":"a"}}}}"#
            ),
            format!(
                r#"{{"schema":2,"seq":4,"at":"2026-09-26T10:00:02Z","event":{{"type":"run_finished","run":"{run}","success":true,"status":"ready"}}}}"#
            ),
        ];
        let events: Vec<Event> = lines
            .iter()
            .map(|l| serde_json::from_str::<Envelope>(l).unwrap().event)
            .collect();
        match &events[0] {
            Event::AgentStarted { model, .. } => assert_eq!(model, ""),
            e => panic!("unexpected {e:?}"),
        }
        match &events[1] {
            Event::ToolCalled { call, subtask, .. } => {
                assert!(call.is_nil());
                assert_eq!(*subtask, None);
            }
            e => panic!("unexpected {e:?}"),
        }
        match &events[2] {
            Event::ToolReturned {
                call,
                exit_code,
                timed_out,
                output_chars,
                output_file,
                ..
            } => {
                assert!(call.is_nil());
                assert_eq!(*exit_code, None);
                assert!(!timed_out);
                assert_eq!(*output_chars, 0);
                assert_eq!(*output_file, None);
            }
            e => panic!("unexpected {e:?}"),
        }
        match &events[3] {
            Event::RunFinished {
                usage,
                active_ms,
                started_at,
                ..
            } => {
                assert_eq!(*usage, Usage::default());
                assert_eq!(*active_ms, 0);
                assert_eq!(*started_at, DateTime::<Utc>::default());
            }
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn new_events_roundtrip() {
        let run = RunId::new();
        for event in [
            Event::Committed {
                run,
                subtask: Some(SubtaskId::new()),
                commit: "abc".into(),
                message: "vibe: s1".into(),
                files: vec!["src/lib.rs".into()],
            },
            Event::Merged {
                run,
                commit: "def".into(),
                branch: "vibe/x-1".into(),
                base: "main".into(),
            },
            Event::ToolReturned {
                run,
                role: crate::AgentRole::Coder,
                tool: "bash".into(),
                is_error: true,
                duration_ms: 1,
                preview: String::new(),
                call: CallId::new(),
                subtask: None,
                exit_code: Some(2),
                timed_out: false,
                output_chars: 10,
                output_file: Some(".vibe/tool-output/001-x/r/c.txt".into()),
            },
        ] {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["type"], event.type_name());
            assert!(Event::TYPES.contains(&event.type_name()));
            assert_eq!(event.run_id(), Some(run));
            let back: Event = serde_json::from_value(json).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn every_event_type_is_documented() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/src/reference/events.md"
        ))
        .expect("docs/src/reference/events.md");
        for name in Event::TYPES {
            assert!(
                doc.contains(&format!("| `{name}` |")),
                "event `{name}` is missing from docs/src/reference/events.md"
            );
        }
    }

    /// The macOS app's tests read this fixture to check that every event
    /// type has an Activity group. `VIBE_UPDATE_FIXTURES=1` rewrites it.
    #[test]
    fn event_types_fixture_is_up_to_date() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/macos/VibeFactory/Packages/VibeAPI/Tests/VibeAPITests/Fixtures/event-types.json"
        );
        let expected = serde_json::to_string_pretty(Event::TYPES).unwrap() + "\n";
        if std::env::var("VIBE_UPDATE_FIXTURES").as_deref() == Ok("1") {
            std::fs::write(path, &expected).expect("write event-types.json");
            return;
        }
        // Compared as JSON: a checkout with CRLF line endings (Windows) still matches.
        let committed = std::fs::read_to_string(path).expect("read event-types.json");
        let committed: Vec<String> =
            serde_json::from_str(&committed).expect("event-types.json is a JSON array");
        assert!(
            committed == Event::TYPES,
            "{path} differs from Event::TYPES; rerun with VIBE_UPDATE_FIXTURES=1 \
             and classify the new types in the app's EventGroup"
        );
    }
}
