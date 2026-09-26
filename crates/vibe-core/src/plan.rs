//! Implementation plans: ordered, possibly parallel, subtasks.

use crate::ids::{SubtaskId, TaskId};

/// State of one subtask.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskStatus {
    /// Not started.
    #[default]
    Pending,
    /// An agent is working on it.
    InProgress,
    /// Implemented.
    Done,
    /// Gave up after retries.
    Failed,
    /// Skipped (dependency failed or no longer needed).
    Skipped,
}

/// One concrete unit of implementation work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Subtask {
    /// Unique identifier.
    pub id: SubtaskId,
    /// Short title.
    pub title: String,
    /// What to do, precisely enough for a fresh agent.
    pub description: String,
    /// Files expected to be touched.
    #[serde(default)]
    pub files: Vec<String>,
    /// Subtasks that must be done first.
    #[serde(default)]
    pub depends_on: Vec<SubtaskId>,
    /// How to verify the subtask (commands, checks).
    #[serde(default)]
    pub verification: Vec<String>,
    /// Current state.
    #[serde(default)]
    pub status: SubtaskStatus,
    /// Number of attempts so far.
    #[serde(default)]
    pub attempts: u32,
    /// Notes left by the implementing agent.
    #[serde(default)]
    pub notes: String,
}

impl Subtask {
    /// Create a pending subtask.
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: SubtaskId::new(),
            title: title.into(),
            description: description.into(),
            files: Vec::new(),
            depends_on: Vec::new(),
            verification: Vec::new(),
            status: SubtaskStatus::Pending,
            attempts: 0,
            notes: String::new(),
        }
    }
}

/// A group of subtasks. Subtasks within a phase may run in parallel when
/// `parallel` is true; phases always run sequentially.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanPhase {
    /// Human readable name.
    pub name: String,
    /// Whether the subtasks of this phase are independent.
    #[serde(default)]
    pub parallel: bool,
    /// The subtasks.
    #[serde(default)]
    pub subtasks: Vec<Subtask>,
}

/// The implementation plan of a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    /// Task this plan belongs to.
    pub task_id: TaskId,
    /// Overall approach in a few sentences.
    #[serde(default)]
    pub approach: String,
    /// Ordered phases.
    #[serde(default)]
    pub phases: Vec<PlanPhase>,
}

impl Plan {
    /// Iterate over every subtask in plan order.
    pub fn subtasks(&self) -> impl Iterator<Item = &Subtask> {
        self.phases.iter().flat_map(|p| p.subtasks.iter())
    }

    /// Mutable iteration over every subtask in plan order.
    pub fn subtasks_mut(&mut self) -> impl Iterator<Item = &mut Subtask> {
        self.phases.iter_mut().flat_map(|p| p.subtasks.iter_mut())
    }

    /// Find a subtask by id.
    #[must_use]
    pub fn subtask(&self, id: SubtaskId) -> Option<&Subtask> {
        self.subtasks().find(|s| s.id == id)
    }

    /// Find a subtask by id, mutably.
    pub fn subtask_mut(&mut self, id: SubtaskId) -> Option<&mut Subtask> {
        self.subtasks_mut().find(|s| s.id == id)
    }

    /// Whether every subtask is done or skipped.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.subtasks()
            .all(|s| matches!(s.status, SubtaskStatus::Done | SubtaskStatus::Skipped))
    }

    /// Total number of subtasks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.subtasks().count()
    }

    /// Whether the plan has no subtasks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Validate that dependencies reference existing subtasks and contain no
    /// cycle.
    pub fn validate(&self) -> crate::Result<()> {
        use std::collections::{HashMap, HashSet};
        let ids: HashSet<SubtaskId> = self.subtasks().map(|s| s.id).collect();
        let deps: HashMap<SubtaskId, Vec<SubtaskId>> = self
            .subtasks()
            .map(|s| (s.id, s.depends_on.clone()))
            .collect();
        for (id, ds) in &deps {
            for d in ds {
                if !ids.contains(d) {
                    return Err(crate::Error::config(format!(
                        "subtask {id} depends on unknown subtask {d}"
                    )));
                }
            }
        }
        // Kahn's algorithm for cycle detection.
        let mut indegree: HashMap<SubtaskId, usize> =
            ids.iter().map(|id| (*id, deps[id].len())).collect();
        let mut queue: Vec<SubtaskId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut seen = 0;
        while let Some(n) = queue.pop() {
            seen += 1;
            for (id, ds) in &deps {
                if ds.contains(&n) {
                    let e = indegree.get_mut(id).expect("known id");
                    *e -= 1;
                    if *e == 0 {
                        queue.push(*id);
                    }
                }
            }
        }
        if seen != ids.len() {
            return Err(crate::Error::config("plan contains a dependency cycle"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(subtasks: Vec<Subtask>) -> Plan {
        Plan {
            task_id: TaskId::new(),
            approach: String::new(),
            phases: vec![PlanPhase {
                name: "p".into(),
                parallel: true,
                subtasks,
            }],
        }
    }

    #[test]
    fn validate_detects_cycle() {
        let mut a = Subtask::new("a", "");
        let mut b = Subtask::new("b", "");
        a.depends_on.push(b.id);
        b.depends_on.push(a.id);
        assert!(plan_with(vec![a, b]).validate().is_err());
    }

    #[test]
    fn validate_accepts_dag() {
        let a = Subtask::new("a", "");
        let mut b = Subtask::new("b", "");
        b.depends_on.push(a.id);
        let p = plan_with(vec![a, b]);
        assert!(p.validate().is_ok());
        assert_eq!(p.len(), 2);
        assert!(!p.is_complete());
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let mut a = Subtask::new("a", "");
        a.depends_on.push(SubtaskId::new());
        assert!(plan_with(vec![a]).validate().is_err());
    }
}
