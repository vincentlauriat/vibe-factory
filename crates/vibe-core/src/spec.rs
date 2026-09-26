//! Specifications: what must be built and how we will know it is done.

use crate::ids::TaskId;

/// Nature of a requirement.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    /// Observable behaviour.
    #[default]
    Functional,
    /// Performance, security, compatibility, …
    NonFunctional,
    /// Explicit boundary: what must **not** change.
    Constraint,
}

/// One requirement extracted from the task description.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Requirement {
    /// Stable short identifier (`R1`, `R2`, …).
    pub id: String,
    /// What is required.
    pub description: String,
    /// Kind of requirement.
    #[serde(default)]
    pub kind: RequirementKind,
    /// 1 = must have, higher = nicer to have.
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// How a reviewer can verify it.
    #[serde(default)]
    pub acceptance: Vec<String>,
}

fn default_priority() -> u8 {
    1
}

/// Codebase context gathered while writing the spec.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SpecContext {
    /// Files judged relevant to the task.
    #[serde(default)]
    pub relevant_files: Vec<String>,
    /// Conventions, patterns and gotchas discovered in the repository.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Open questions or assumptions the agents made.
    #[serde(default)]
    pub assumptions: Vec<String>,
}

/// The specification of a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spec {
    /// Task this spec belongs to.
    pub task_id: TaskId,
    /// One paragraph summary of the goal.
    pub summary: String,
    /// The requirements.
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// Gathered context.
    #[serde(default)]
    pub context: SpecContext,
    /// Free-form markdown body (design notes, API sketches, …).
    #[serde(default)]
    pub body: String,
}

impl Spec {
    /// Render the spec as a markdown document.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("# Specification\n\n");
        md.push_str(&self.summary);
        md.push_str("\n\n## Requirements\n\n");
        for r in &self.requirements {
            md.push_str(&format!(
                "- **{}** (p{}, {:?}): {}\n",
                r.id, r.priority, r.kind, r.description
            ));
            for a in &r.acceptance {
                md.push_str(&format!("  - ✓ {a}\n"));
            }
        }
        if !self.context.relevant_files.is_empty() {
            md.push_str("\n## Relevant files\n\n");
            for f in &self.context.relevant_files {
                md.push_str(&format!("- `{f}`\n"));
            }
        }
        if !self.context.findings.is_empty() {
            md.push_str("\n## Findings\n\n");
            for f in &self.context.findings {
                md.push_str(&format!("- {f}\n"));
            }
        }
        if !self.context.assumptions.is_empty() {
            md.push_str("\n## Assumptions\n\n");
            for a in &self.context.assumptions {
                md.push_str(&format!("- {a}\n"));
            }
        }
        if !self.body.is_empty() {
            md.push_str("\n## Notes\n\n");
            md.push_str(&self.body);
            md.push('\n');
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_lists_requirements() {
        let spec = Spec {
            task_id: TaskId::new(),
            summary: "Do the thing".into(),
            requirements: vec![Requirement {
                id: "R1".into(),
                description: "It works".into(),
                kind: RequirementKind::Functional,
                priority: 1,
                acceptance: vec!["tests pass".into()],
            }],
            context: SpecContext::default(),
            body: String::new(),
        };
        let md = spec.to_markdown();
        assert!(md.contains("**R1**"));
        assert!(md.contains("tests pass"));
    }
}
