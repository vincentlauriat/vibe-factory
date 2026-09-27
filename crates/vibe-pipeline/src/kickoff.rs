//! Kickoff (first user) messages of each agent role, and truncation helpers.
//!
//! A kickoff message is made of clearly delimited sections (`## TASK`,
//! `## SPEC`, `## PLAN`, `## SUBTASK`, `## PROGRESS NOTES`, `## MEMORY`,
//! `## PRIOR CONTEXT`, `## INSTRUCTIONS`). Built-in system prompts already
//! embed most of this material through `{{placeholders}}`; to avoid sending
//! the same text twice, a section whose placeholder is consumed by the
//! agent's system prompt is replaced by a one-line pointer. Custom prompts
//! without placeholders therefore still receive every piece of context.

use vibe_core::{AgentRole, PromptTemplate, Task};

/// Maximum characters of prior phase output handed to the next agent.
pub const PRIOR_CONTEXT_MAX_CHARS: usize = 12_000;
/// Maximum characters of progress notes (the most recent part is kept).
pub const PROGRESS_MAX_CHARS: usize = 8_000;
/// Maximum characters of memory notes.
pub const MEMORY_MAX_CHARS: usize = 6_000;
/// Maximum characters of the workspace change summary shown to QA.
pub const CHANGES_MAX_CHARS: usize = 20_000;
/// Maximum characters of a rendered spec or plan.
pub const DOCUMENT_MAX_CHARS: usize = 24_000;

fn char_boundary(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(i, _)| i)
}

/// Keep the first `max` characters of `s`, appending a truncation marker.
#[must_use]
pub fn truncate_head(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    format!(
        "{}\n[… truncated: {} more characters]",
        &s[..char_boundary(s, max)],
        total - max
    )
}

/// Keep the last `max` characters of `s`, prepending a truncation marker.
/// Used for append-only logs where the recent part matters most.
#[must_use]
pub fn truncate_tail(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    format!(
        "[… {} earlier characters truncated]\n{}",
        total - max,
        &s[char_boundary(s, total - max)..]
    )
}

/// Keep the beginning and the end of `s` (half of `max` each).
#[must_use]
pub fn truncate_middle(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    let half = max / 2;
    format!(
        "{}\n[… {} characters truncated …]\n{}",
        &s[..char_boundary(s, half)],
        total - 2 * half,
        &s[char_boundary(s, total - half)..]
    )
}

/// Builder of a kickoff message.
#[derive(Debug, Clone, Default)]
pub struct Kickoff {
    intro: String,
    sections: Vec<(String, Option<String>, String)>,
    instructions: String,
}

impl Kickoff {
    /// New message starting with `intro`.
    pub fn new(intro: impl Into<String>) -> Self {
        Self {
            intro: intro.into(),
            ..Self::default()
        }
    }

    /// Section always included in full. Empty content renders as `(none)`.
    #[must_use]
    pub fn section(mut self, title: &str, content: impl Into<String>) -> Self {
        self.sections
            .push((title.to_string(), None, content.into()));
        self
    }

    /// Section backed by the prompt variable `var`: replaced by a pointer
    /// when the system prompt already consumes `{{var}}`.
    #[must_use]
    pub fn var_section(mut self, title: &str, var: &str, content: impl Into<String>) -> Self {
        self.sections
            .push((title.to_string(), Some(var.to_string()), content.into()));
        self
    }

    /// Final `INSTRUCTIONS` section.
    #[must_use]
    pub fn instructions(mut self, text: impl Into<String>) -> Self {
        self.instructions = text.into();
        self
    }

    /// Render against the (unrendered) system prompt template of the agent.
    #[must_use]
    pub fn build(&self, system_prompt: &str) -> String {
        let used = PromptTemplate::new(system_prompt).placeholders();
        let mut out = String::new();
        if !self.intro.trim().is_empty() {
            out.push_str(self.intro.trim());
            out.push_str("\n\n");
        }
        for (title, var, content) in &self.sections {
            out.push_str(&format!("## {title}\n\n"));
            match var {
                Some(v) if used.iter().any(|u| u == v) => out.push_str(&format!(
                    "(See the {} section of your instructions.)\n\n",
                    title.to_lowercase()
                )),
                _ if content.trim().is_empty() => out.push_str("(none)\n\n"),
                _ => {
                    out.push_str(content.trim_end());
                    out.push_str("\n\n");
                }
            }
        }
        if !self.instructions.trim().is_empty() {
            out.push_str("## INSTRUCTIONS\n\n");
            out.push_str(self.instructions.trim());
            out.push('\n');
        }
        out.trim_end().to_string() + "\n"
    }
}

/// Material available to a kickoff message. Empty strings mean "none".
#[derive(Debug, Clone, Copy)]
pub struct KickoffData<'a> {
    /// The task.
    pub task: &'a Task,
    /// Rendered specification.
    pub spec: &'a str,
    /// Rendered plan.
    pub plan: &'a str,
    /// Rendered subtask (coder roles).
    pub subtask: &'a str,
    /// Progress notes.
    pub progress: &'a str,
    /// Memory notes.
    pub memory: &'a str,
    /// Output of earlier phases or attempts.
    pub prior_context: &'a str,
    /// Workspace changes (QA).
    pub changes: &'a str,
    /// Latest QA report (fixer).
    pub qa_report: &'a str,
}

impl<'a> KickoffData<'a> {
    /// Data holding only the task.
    #[must_use]
    pub fn new(task: &'a Task) -> Self {
        Self {
            task,
            spec: "",
            plan: "",
            subtask: "",
            progress: "",
            memory: "",
            prior_context: "",
            changes: "",
            qa_report: "",
        }
    }
}

/// Short reminder of the JSON document each built-in role must end with.
#[must_use]
pub fn output_instructions(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::ComplexityAssessor => {
            "Classify the task. End with the JSON document {complexity, confidence, reasoning, needs_research, needs_critique, risk_level}."
        }
        AgentRole::SpecGatherer => {
            "Explore the codebase and extract the requirements. End with the JSON document {summary, requirements, context}."
        }
        AgentRole::SpecResearcher => {
            "Validate the external knowledge the requirements rely on. Answer with the markdown research report."
        }
        AgentRole::SpecWriter => {
            "Write the specification file, then end with the JSON document {summary, requirements, context, body}."
        }
        AgentRole::SpecCritic => {
            "Critique the specification. End with the JSON document {verdict, issues, spec}."
        }
        AgentRole::Planner => {
            "Produce the implementation plan. End with the JSON document {approach, phases: [{name, parallel, subtasks: [{title, description, files, depends_on, verification}]}]}. `depends_on` lists titles of earlier subtasks."
        }
        AgentRole::Coder | AgentRole::CoderRecovery => {
            "Implement and verify the subtask. End with the JSON document {status: \"done\" | \"failed\", summary, files_changed, notes}."
        }
        AgentRole::QaReviewer => {
            "Review the implementation against every acceptance criterion. End with the JSON document {verdict, summary, issues}."
        }
        AgentRole::QaFixer => {
            "Fix every issue of the QA report. End with the JSON document {status, summary, fixed}."
        }
        _ => "Complete the work described above.",
    }
}

/// Kickoff message for `role`, rendered against the agent's system prompt
/// template (see the module documentation).
#[must_use]
pub fn kickoff_for(role: &AgentRole, data: &KickoffData<'_>, system_prompt: &str) -> String {
    let task = format!("**{}**\n\n{}", data.task.title, data.task.description);
    let intro = match role {
        AgentRole::Coder => "Implement the subtask below.".to_string(),
        AgentRole::CoderRecovery => {
            "Earlier attempts at the subtask below failed. Recover it.".into()
        }
        AgentRole::QaReviewer => "Review the implementation of the task below.".into(),
        AgentRole::QaFixer => "Fix the issues reported by QA for the task below.".into(),
        other => format!("You are the `{other}` agent for the task below."),
    };
    let mut k = Kickoff::new(intro).var_section("TASK", "task_description", task);
    let is = |r: &[AgentRole]| r.contains(role);
    use AgentRole as R;
    if is(&[R::Coder, R::CoderRecovery]) {
        k = k.var_section("SUBTASK", "subtask", data.subtask);
    }
    if is(&[
        R::SpecCritic,
        R::Planner,
        R::Coder,
        R::CoderRecovery,
        R::QaReviewer,
        R::QaFixer,
    ]) {
        k = k.var_section("SPEC", "spec", data.spec);
    }
    if is(&[R::Coder, R::CoderRecovery, R::QaReviewer]) {
        k = k.var_section("PLAN", "plan", data.plan);
        k = k.var_section("PROGRESS NOTES", "progress", data.progress);
    }
    if is(&[R::QaReviewer]) {
        k = k.var_section("CHANGES", "changes", data.changes);
    }
    if is(&[R::QaFixer]) {
        k = k.var_section("QA REPORT", "qa_report", data.qa_report);
    }
    if !is(&[R::ComplexityAssessor]) {
        k = k.var_section("MEMORY", "memory", data.memory);
    }
    k = k.var_section("PRIOR CONTEXT", "prior_context", data.prior_context);
    k.instructions(output_instructions(role))
        .build(system_prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_helpers() {
        assert_eq!(truncate_head("abc", 5), "abc");
        let h = truncate_head("héllo world", 2);
        assert!(h.starts_with("hé\n") && h.contains("9 more"));
        let t = truncate_tail("abcdef", 2);
        assert!(t.ends_with("\nef") && t.contains("4 earlier"));
        let m = truncate_middle("abcdefghij", 4);
        assert!(m.starts_with("ab\n") && m.ends_with("\nij"));
        assert_eq!(truncate_middle("ab", 4), "ab");
    }

    #[test]
    fn consumed_placeholders_become_pointers() {
        let task = Task::new("Title", "Body");
        let mut data = KickoffData::new(&task);
        data.spec = "THE SPEC";
        data.subtask = "THE SUBTASK";
        data.changes = "THE DIFF";
        let with_vars = kickoff_for(
            &AgentRole::Coder,
            &data,
            "{{task_description}} {{spec}} {{subtask}}",
        );
        assert!(!with_vars.contains("THE SPEC"));
        assert!(with_vars.contains("(See the spec section"));
        assert!(with_vars.contains("## INSTRUCTIONS"));
        let bare = kickoff_for(&AgentRole::Coder, &data, "no placeholders");
        assert!(bare.contains("THE SPEC") && bare.contains("THE SUBTASK"));
        assert!(bare.contains("**Title**"));
        assert!(bare.contains("## PROGRESS NOTES\n\n(none)"));
        let qa = kickoff_for(
            &AgentRole::QaReviewer,
            &data,
            vibe_agents::prompts::QA_REVIEWER,
        );
        assert!(
            qa.contains("THE DIFF"),
            "QA prompt has no {{changes}}: diff must be inlined"
        );
    }
}
