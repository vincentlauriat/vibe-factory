//! Built-in system prompts, one markdown file per role, embedded at compile
//! time.
//!
//! Every prompt starts with an HTML comment listing the variables it uses
//! (`- name: description`). That comment is documentation only: it is removed
//! by [`strip_doc_comment`] before the prompt is rendered, so it never reaches
//! the model. Variables are written `{{name}}` in the body and rendered with
//! [`vibe_core::PromptTemplate`]; unknown variables render as an empty string.

use vibe_core::AgentRole;

/// System prompt of the complexity assessor.
pub const COMPLEXITY_ASSESSOR: &str = include_str!("complexity_assessor.md");
/// System prompt of the spec gatherer.
pub const SPEC_GATHERER: &str = include_str!("spec_gatherer.md");
/// System prompt of the spec researcher.
pub const SPEC_RESEARCHER: &str = include_str!("spec_researcher.md");
/// System prompt of the spec writer.
pub const SPEC_WRITER: &str = include_str!("spec_writer.md");
/// System prompt of the spec critic.
pub const SPEC_CRITIC: &str = include_str!("spec_critic.md");
/// System prompt of the planner.
pub const PLANNER: &str = include_str!("planner.md");
/// System prompt of the coder.
pub const CODER: &str = include_str!("coder.md");
/// System prompt of the coder recovery agent.
pub const CODER_RECOVERY: &str = include_str!("coder_recovery.md");
/// System prompt of the QA reviewer.
pub const QA_REVIEWER: &str = include_str!("qa_reviewer.md");
/// System prompt of the QA fixer.
pub const QA_FIXER: &str = include_str!("qa_fixer.md");
/// System prompt of the merge resolver.
pub const MERGE_RESOLVER: &str = include_str!("merge_resolver.md");
/// System prompt of the commit message writer.
pub const COMMIT_MESSAGE: &str = include_str!("commit_message.md");

/// Variables provided by [`crate::AgentRunner`] for every agent.
pub const COMMON_VARIABLES: &[&str] = &["task_title", "task_description", "workspace_root", "date"];

/// Raw built-in prompt (including its documentation comment) for a role.
#[must_use]
pub fn builtin_prompt(role: &AgentRole) -> Option<&'static str> {
    Some(match role {
        AgentRole::ComplexityAssessor => COMPLEXITY_ASSESSOR,
        AgentRole::SpecGatherer => SPEC_GATHERER,
        AgentRole::SpecResearcher => SPEC_RESEARCHER,
        AgentRole::SpecWriter => SPEC_WRITER,
        AgentRole::SpecCritic => SPEC_CRITIC,
        AgentRole::Planner => PLANNER,
        AgentRole::Coder => CODER,
        AgentRole::CoderRecovery => CODER_RECOVERY,
        AgentRole::QaReviewer => QA_REVIEWER,
        AgentRole::QaFixer => QA_FIXER,
        AgentRole::MergeResolver => MERGE_RESOLVER,
        AgentRole::CommitMessage => COMMIT_MESSAGE,
        AgentRole::Custom(_) => return None,
    })
}

/// Remove a leading `<!-- … -->` documentation comment, if any.
#[must_use]
pub fn strip_doc_comment(prompt: &str) -> &str {
    let trimmed = prompt.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<!--")
        && let Some(end) = rest.find("-->")
    {
        return rest[end + 3..].trim_start();
    }
    prompt
}

/// Variables documented in the leading comment of a prompt, as
/// `(name, description)` pairs, in order.
#[must_use]
pub fn documented_variables(prompt: &str) -> Vec<(String, String)> {
    let trimmed = prompt.trim_start();
    let Some(rest) = trimmed.strip_prefix("<!--") else {
        return Vec::new();
    };
    let Some(end) = rest.find("-->") else {
        return Vec::new();
    };
    rest[..end]
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter_map(|item| item.split_once(':'))
        .map(|(name, desc)| (name.trim().to_string(), desc.trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use vibe_core::PromptTemplate;

    const ALL_ROLES: [AgentRole; 12] = [
        AgentRole::ComplexityAssessor,
        AgentRole::SpecGatherer,
        AgentRole::SpecResearcher,
        AgentRole::SpecWriter,
        AgentRole::SpecCritic,
        AgentRole::Planner,
        AgentRole::Coder,
        AgentRole::CoderRecovery,
        AgentRole::QaReviewer,
        AgentRole::QaFixer,
        AgentRole::MergeResolver,
        AgentRole::CommitMessage,
    ];

    #[test]
    fn every_role_has_a_prompt() {
        for role in &ALL_ROLES {
            assert!(builtin_prompt(role).is_some(), "{role}");
        }
        assert!(builtin_prompt(&AgentRole::Custom("x".into())).is_none());
    }

    #[test]
    fn documented_variables_match_placeholders_exactly() {
        for role in &ALL_ROLES {
            let raw = builtin_prompt(role).unwrap();
            let documented: BTreeSet<String> = documented_variables(raw)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            assert!(documented.contains("task_title"), "{role}");
            let used: BTreeSet<String> = PromptTemplate::new(strip_doc_comment(raw))
                .placeholders()
                .into_iter()
                .collect();
            let undocumented: Vec<_> = used.difference(&documented).collect();
            assert!(undocumented.is_empty(), "{role}: {undocumented:?}");
            let unused: Vec<_> = documented.difference(&used).collect();
            assert!(
                unused.is_empty(),
                "{role}: documented but unused {unused:?}"
            );
        }
    }

    #[test]
    fn rendered_prompts_are_clean() {
        for role in &ALL_ROLES {
            let raw = builtin_prompt(role).unwrap();
            let vars: BTreeMap<String, String> = documented_variables(raw)
                .into_iter()
                .map(|(n, _)| (n.clone(), format!("<{n}>")))
                .collect();
            let out = PromptTemplate::new(strip_doc_comment(raw)).render(&vars);
            assert!(!out.contains("{{"), "{role}: stray placeholder");
            assert!(!out.contains("<!--"), "{role}: doc comment leaked");
            assert!(out.contains("<task_title>"), "{role}");
        }
    }

    #[test]
    fn doc_comment_handling() {
        assert_eq!(strip_doc_comment("<!--\n- a: b\n-->\nHello"), "Hello");
        assert_eq!(strip_doc_comment("Hello"), "Hello");
        assert_eq!(strip_doc_comment("<!-- unterminated"), "<!-- unterminated");
        assert_eq!(
            documented_variables("<!--\nVariables:\n- a: first\n- b: second: x\n-->"),
            vec![
                ("a".to_string(), "first".to_string()),
                ("b".to_string(), "second: x".to_string())
            ]
        );
        assert!(documented_variables("no comment").is_empty());
    }
}
