//! Persistent state of a pipeline run (`run.json`).
//!
//! The state is written before and after every phase so that an interrupted
//! run (crash, `Ctrl-C`, hook veto, dry run) can be resumed with
//! [`crate::Pipeline::resume`] from [`RunState::current_phase`].

use chrono::{DateTime, Utc};
use vibe_core::{Phase, RunId, TaskId};

use crate::complexity::Profile;

/// Lifecycle of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// A phase is executing (or the process died while it was).
    Running,
    /// The run stopped on purpose before the end (dry run, `until_phase`,
    /// QA escalation) and may be resumed.
    Paused,
    /// The run reached the end of its profile.
    Finished,
    /// A phase failed.
    Failed,
    /// The run was cancelled by the user or a hook.
    Cancelled,
}

impl RunStatus {
    /// Whether [`crate::Pipeline::resume`] can continue a run in this state.
    #[must_use]
    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            RunStatus::Running | RunStatus::Paused | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// State of one pipeline run, persisted as `run.json` in the task directory.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunState {
    /// Run identifier (kept across resumes).
    pub run_id: RunId,
    /// Task being run.
    pub task_id: TaskId,
    /// Phase being executed, or the next phase to execute when the run is
    /// not [`RunStatus::Running`].
    pub current_phase: Phase,
    /// Phases that completed successfully, in execution order (QA and Fix
    /// may appear several times).
    #[serde(default)]
    pub completed_phases: Vec<Phase>,
    /// Number of QA reviews performed so far.
    #[serde(default)]
    pub qa_round: u32,
    /// Profile chosen after the assessment, once known.
    #[serde(default)]
    pub profile: Option<Profile>,
    /// When the run started.
    pub started_at: DateTime<Utc>,
    /// Last update.
    pub updated_at: DateTime<Utc>,
    /// Lifecycle status.
    pub status: RunStatus,
    /// Last error message, if the run failed.
    #[serde(default)]
    pub last_error: Option<String>,
}

impl RunState {
    /// Fresh state for a new run starting at `phase`.
    #[must_use]
    pub fn new(run_id: RunId, task_id: TaskId, phase: Phase) -> Self {
        let now = Utc::now();
        Self {
            run_id,
            task_id,
            current_phase: phase,
            completed_phases: Vec::new(),
            qa_round: 0,
            profile: None,
            started_at: now,
            updated_at: now,
            status: RunStatus::Running,
            last_error: None,
        }
    }

    /// Bump `updated_at`.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Whether `phase` completed at least once in this run.
    #[must_use]
    pub fn has_completed(&self, phase: Phase) -> bool {
        self.completed_phases.contains(&phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_resumable() {
        let mut s = RunState::new(RunId::new(), TaskId::new(), Phase::Assess);
        s.completed_phases.push(Phase::Assess);
        s.status = RunStatus::Paused;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"paused\""));
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert!(back.has_completed(Phase::Assess));
        assert!(RunStatus::Paused.is_resumable());
        assert!(!RunStatus::Finished.is_resumable());
    }
}
