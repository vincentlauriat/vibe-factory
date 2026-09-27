//! Complexity classification and the pipeline profile derived from it.
//!
//! | Complexity | Phases after `assess` | Spec steps |
//! |------------|-----------------------|------------|
//! | trivial    | plan, build, qa, merge | none: the planner works from the task description |
//! | simple     | spec, plan, build, qa, fix, merge | gatherer only (light spec) |
//! | standard   | spec, plan, build, qa, fix, merge | gatherer + writer |
//! | complex    | spec, plan, build, qa, fix, merge | gatherer + researcher + writer + critic |
//!
//! The complexity comes from, in order: the `--complexity` override
//! ([`crate::RunOptions::complexity_override`]), the complexity already
//! stored on the task, [`heuristic_complexity`], and finally the
//! `complexity_assessor` agent ([`AssessmentOutput`]). When the agent fails,
//! the pipeline falls back to [`Complexity::Standard`].

use std::sync::OnceLock;

use regex::Regex;
use vibe_core::{Complexity, Phase, Task};

/// What the pipeline runs for a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// Phases run after [`Phase::Assess`], in order. [`Phase::Fix`] is only
    /// entered when QA requests changes. A pending required-validation failure can
    /// also enter Fix even when this list omits it.
    pub phases: Vec<Phase>,
    /// Run the `spec_researcher` step.
    pub research: bool,
    /// Run the `spec_critic` step.
    pub critique: bool,
    /// Run the `spec_writer` step; when false the spec is built from the
    /// gatherer output alone (light spec).
    #[serde(default = "default_true")]
    pub full_spec: bool,
}

fn default_true() -> bool {
    true
}

impl Profile {
    /// Whether the profile contains `phase`.
    #[must_use]
    pub fn has(&self, phase: Phase) -> bool {
        phase == Phase::Assess || self.phases.contains(&phase)
    }

    /// The phase following `phase` in canonical order that the profile
    /// runs, skipping [`Phase::Fix`] (which is reached by an explicit QA or validation transition).
    #[must_use]
    pub fn next_after(&self, phase: Phase) -> Option<Phase> {
        Phase::ALL
            .into_iter()
            .filter(|p| *p > phase && *p != Phase::Fix)
            .find(|p| self.has(*p))
    }
}

/// Profile for a complexity class (see the module table).
#[must_use]
pub fn profile_for(complexity: Complexity) -> Profile {
    use Phase::{Build, Fix, Merge, Plan, Qa, Spec};
    match complexity {
        Complexity::Trivial => Profile {
            phases: vec![Plan, Build, Qa, Merge],
            research: false,
            critique: false,
            full_spec: false,
        },
        Complexity::Simple => Profile {
            phases: vec![Spec, Plan, Build, Qa, Fix, Merge],
            research: false,
            critique: false,
            full_spec: false,
        },
        Complexity::Standard => Profile {
            phases: vec![Spec, Plan, Build, Qa, Fix, Merge],
            research: false,
            critique: false,
            full_spec: true,
        },
        Complexity::Complex => Profile {
            phases: vec![Spec, Plan, Build, Qa, Fix, Merge],
            research: true,
            critique: true,
            full_spec: true,
        },
    }
}

/// Maximum number of words for the heuristic fast path.
pub const HEURISTIC_MAX_WORDS: usize = 30;

fn trivial_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(fix(es|ed)?\s+(a\s+|the\s+)?typos?|typos?|bump\s+(the\s+)?(version|dependency|deps?)|change\s+(the\s+)?(colou?r|text|label|wording|title|copy)|update\s+(the\s+)?(copyright|year))\b",
        )
        .expect("valid regex")
    })
}

fn simple_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(rename[sd]?|remove\s+(the\s+)?unused|delete\s+(the\s+)?unused|fix\s+(the\s+)?(lint|clippy|compiler)?\s*warnings?|add\s+(a\s+)?comment)\b",
        )
        .expect("valid regex")
    })
}

/// Fast classification without any model call.
///
/// Returns `Some` only for short tasks (at most [`HEURISTIC_MAX_WORDS`]
/// words across title and description) matching a well-known small-change
/// pattern: typos, version bumps, color/text/label changes are
/// [`Complexity::Trivial`]; renames, removal of unused code and warning
/// fixes are [`Complexity::Simple`].
#[must_use]
pub fn heuristic_complexity(task: &Task) -> Option<Complexity> {
    let text = format!("{} {}", task.title, task.description);
    if text.split_whitespace().count() > HEURISTIC_MAX_WORDS {
        return None;
    }
    if trivial_patterns().is_match(&text) {
        Some(Complexity::Trivial)
    } else if simple_patterns().is_match(&text) {
        Some(Complexity::Simple)
    } else {
        None
    }
}

/// Structured output of the `complexity_assessor` agent.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssessmentOutput {
    /// `trivial`, `simple`, `standard` or `complex` (aliases accepted, see
    /// [`parse_complexity`]).
    pub complexity: String,
    /// Confidence between 0 and 1.
    #[serde(default)]
    pub confidence: f64,
    /// Short justification.
    #[serde(default)]
    pub reasoning: String,
    /// The task depends on external knowledge.
    #[serde(default)]
    pub needs_research: bool,
    /// A critique of the spec would likely help.
    #[serde(default, alias = "needs_self_critique")]
    pub needs_critique: bool,
    /// `low`, `medium` or `high`.
    #[serde(default)]
    pub risk_level: String,
}

/// Parse a complexity name, tolerating common aliases (`low`, `medium`,
/// `high`, `quick`, `easy`, `hard`, …). Unknown names yield `None`.
#[must_use]
pub fn parse_complexity(s: &str) -> Option<Complexity> {
    match s.trim().to_ascii_lowercase().as_str() {
        "trivial" | "tiny" | "quick" => Some(Complexity::Trivial),
        "simple" | "easy" | "low" | "small" => Some(Complexity::Simple),
        "standard" | "medium" | "moderate" | "normal" => Some(Complexity::Standard),
        "complex" | "high" | "hard" | "large" => Some(Complexity::Complex),
        _ => None,
    }
}

impl AssessmentOutput {
    /// Profile for this assessment: [`profile_for`] of the complexity, with
    /// research and critique switched on when the assessor asks for them
    /// and the profile writes a full spec.
    #[must_use]
    pub fn profile(&self) -> Option<Profile> {
        let complexity = parse_complexity(&self.complexity)?;
        let mut p = profile_for(complexity);
        if p.full_spec {
            p.research |= self.needs_research;
            p.critique |= self.needs_critique;
        }
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn profiles_match_table() {
        let t = profile_for(Complexity::Trivial);
        assert!(!t.has(Phase::Spec));
        assert!(!t.has(Phase::Fix));
        assert_eq!(t.next_after(Phase::Assess), Some(Phase::Plan));
        let s = profile_for(Complexity::Simple);
        assert!(s.has(Phase::Spec) && !s.full_spec && !s.research);
        let st = profile_for(Complexity::Standard);
        assert!(st.full_spec && !st.critique);
        let c = profile_for(Complexity::Complex);
        assert!(c.research && c.critique);
        assert_eq!(c.next_after(Phase::Qa), Some(Phase::Merge));
        assert_eq!(c.next_after(Phase::Merge), None);
    }

    #[test]
    fn heuristic_fast_path() {
        let h = |title: &str, desc: &str| heuristic_complexity(&Task::new(title, desc));
        assert_eq!(h("Fix typo in README", ""), Some(Complexity::Trivial));
        assert_eq!(h("Bump version to 1.2.0", ""), Some(Complexity::Trivial));
        assert_eq!(
            h("Change label", "Change text of the submit button"),
            Some(Complexity::Trivial)
        );
        assert_eq!(
            h("Rename helper", "rename foo to bar"),
            Some(Complexity::Simple)
        );
        assert_eq!(
            h("Cleanup", "remove unused imports in lib.rs"),
            Some(Complexity::Simple)
        );
        assert_eq!(h("Add OAuth login", "Support GitHub OAuth"), None);
        let long = "fix typo ".repeat(20);
        assert_eq!(h("x", &long), None);
    }

    #[test]
    fn assessment_parsing_and_flags() {
        let a: AssessmentOutput = serde_json::from_str(
            r#"{"complexity":"Standard","confidence":0.8,"reasoning":"r","needs_research":true,"needs_self_critique":true,"risk_level":"high"}"#,
        )
        .unwrap();
        let p = a.profile().unwrap();
        assert!(p.research && p.critique && p.full_spec);
        assert_eq!(parse_complexity("HARD"), Some(Complexity::Complex));
        assert_eq!(parse_complexity("??"), None);
        let simple = AssessmentOutput {
            complexity: "simple".into(),
            confidence: 1.0,
            reasoning: String::new(),
            needs_research: true,
            needs_critique: false,
            risk_level: String::new(),
        };
        assert!(!simple.profile().unwrap().research);
    }
}
