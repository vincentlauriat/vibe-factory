//! The run manager: the one seam every user interface drives the engine
//! through (see ADR-007).
//!
//! A [`RunManager`] starts and resumes runs in background tasks, cancels
//! them, lists the active ones and hands out event subscriptions. It never
//! renders anything: the CLI, the terminal UI and a server all use the same
//! operations and the same events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};
use vibe_core::{Envelope, Error, ErrorKind, Result, TaskId};

use crate::pipeline::{Pipeline, RunOptions, RunReport};

/// A run started by a [`RunManager`].
#[derive(Debug)]
pub struct RunHandle {
    task: TaskId,
    join: tokio::task::JoinHandle<Result<RunReport>>,
}

impl RunHandle {
    /// Task being run.
    #[must_use]
    pub fn task(&self) -> TaskId {
        self.task
    }

    /// Wait for the run to end.
    pub async fn wait(self) -> Result<RunReport> {
        match self.join.await {
            Ok(result) => result,
            Err(e) if e.is_cancelled() => Err(Error::new(ErrorKind::Cancelled, "run aborted")),
            Err(e) => Err(Error::other(format!("run panicked: {e}"))),
        }
    }
}

type Active = Arc<Mutex<HashMap<TaskId, watch::Sender<bool>>>>;

/// Starts, resumes, cancels and lists runs of one pipeline.
#[derive(Clone)]
pub struct RunManager {
    pipeline: Arc<Pipeline>,
    active: Active,
}

impl std::fmt::Debug for RunManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunManager")
            .field("active", &self.active())
            .finish_non_exhaustive()
    }
}

impl RunManager {
    /// Manager of the runs of `pipeline`.
    #[must_use]
    pub fn new(pipeline: Pipeline) -> Self {
        Self {
            pipeline: Arc::new(pipeline),
            active: Arc::default(),
        }
    }

    /// The pipeline runs go through (its store and event bus).
    #[must_use]
    pub fn pipeline(&self) -> &Arc<Pipeline> {
        &self.pipeline
    }

    /// Live events of every run of this manager.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.pipeline.events().subscribe()
    }

    /// Start a new run of `task`. The manager owns cancellation:
    /// `options.cancel` is replaced (use [`RunManager::cancel`]).
    pub fn start(&self, task: TaskId, options: RunOptions) -> Result<RunHandle> {
        self.spawn(task, options, false)
    }

    /// Resume the last run of `task` (see [`Pipeline::resume_with`]).
    pub fn resume(&self, task: TaskId, options: RunOptions) -> Result<RunHandle> {
        self.spawn(task, options, true)
    }

    /// Ask the run of `task` to stop after its current step. Returns false
    /// when this manager does not run the task.
    pub fn cancel(&self, task: TaskId) -> bool {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        match active.get(&task) {
            Some(tx) => {
                let _ = tx.send(true);
                true
            }
            None => false,
        }
    }

    /// Whether this manager currently runs `task`.
    #[must_use]
    pub fn is_active(&self, task: TaskId) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&task)
    }

    /// Tasks this manager currently runs.
    #[must_use]
    pub fn active(&self) -> Vec<TaskId> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    fn spawn(&self, task: TaskId, mut options: RunOptions, resume: bool) -> Result<RunHandle> {
        let (tx, rx) = watch::channel(false);
        {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            if active.contains_key(&task) {
                return Err(Error::config(format!("task {task} is already running")));
            }
            active.insert(task, tx);
        }
        options.cancel = Some(rx);
        let pipeline = Arc::clone(&self.pipeline);
        let active = Arc::clone(&self.active);
        let join = tokio::spawn(async move {
            // Removes the task from the active set however the run ends.
            let _done = Deregister { active, task };
            if resume {
                pipeline.resume_with(task, options).await
            } else {
                pipeline.run(task, options).await
            }
        });
        Ok(RunHandle { task, join })
    }
}

struct Deregister {
    active: Active,
    task: TaskId,
}

impl Drop for Deregister {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.task);
    }
}
