//! Events emitted while the framework works, and the bus that carries them.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::agent::AgentRole;
use crate::ids::{RunId, SubtaskId, TaskId};
use crate::phase::Phase;
use crate::provider::Usage;

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
    /// Run this event belongs to, if any.
    #[must_use]
    pub fn run_id(&self) -> Option<RunId> {
        match self {
            Event::RunStarted { run, .. }
            | Event::PhaseStarted { run, .. }
            | Event::PhaseFinished { run, .. }
            | Event::AgentStarted { run, .. }
            | Event::AgentText { run, .. }
            | Event::ToolCalled { run, .. }
            | Event::ToolReturned { run, .. }
            | Event::AgentFinished { run, .. }
            | Event::SubtaskUpdated { run, .. }
            | Event::Retrying { run, .. }
            | Event::Paused { run, .. }
            | Event::RunFinished { run, .. } => Some(*run),
            Event::Log { run, .. } => *run,
        }
    }
}

/// An event with its timestamp.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    /// When the event was published.
    pub at: DateTime<Utc>,
    /// The event.
    pub event: Event,
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
        }
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
        let envelope = Envelope {
            at: Utc::now(),
            event,
        };
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
}
