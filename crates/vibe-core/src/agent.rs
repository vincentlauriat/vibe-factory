//! Declarative agent definitions and the outcome of running one.

use crate::message::Message;
use crate::provider::{ModelRef, Usage};

/// Well-known agent roles used by the default pipeline. Plugins may add
/// arbitrary roles through [`AgentRole::Custom`].
///
/// Roles serialise as plain `snake_case` strings (`"qa_reviewer"`,
/// `"my_custom_agent"`), so the wire format is the same for built-in and
/// custom roles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentRole {
    /// Classifies the task complexity.
    ComplexityAssessor,
    /// Explores the codebase and extracts requirements.
    SpecGatherer,
    /// Researches external libraries and APIs.
    SpecResearcher,
    /// Writes the specification.
    SpecWriter,
    /// Critiques and improves the specification.
    SpecCritic,
    /// Produces the implementation plan.
    Planner,
    /// Implements one subtask.
    Coder,
    /// Recovers a subtask after repeated failures.
    CoderRecovery,
    /// Reviews the implementation.
    QaReviewer,
    /// Fixes QA findings.
    QaFixer,
    /// Resolves merge conflicts.
    MergeResolver,
    /// Writes commit messages.
    CommitMessage,
    /// Any other role provided by configuration or plugins.
    Custom(String),
}

const BUILTIN_ROLES: &[(&str, AgentRole)] = &[
    ("complexity_assessor", AgentRole::ComplexityAssessor),
    ("spec_gatherer", AgentRole::SpecGatherer),
    ("spec_researcher", AgentRole::SpecResearcher),
    ("spec_writer", AgentRole::SpecWriter),
    ("spec_critic", AgentRole::SpecCritic),
    ("planner", AgentRole::Planner),
    ("coder", AgentRole::Coder),
    ("coder_recovery", AgentRole::CoderRecovery),
    ("qa_reviewer", AgentRole::QaReviewer),
    ("qa_fixer", AgentRole::QaFixer),
    ("merge_resolver", AgentRole::MergeResolver),
    ("commit_message", AgentRole::CommitMessage),
];

impl AgentRole {
    /// Every built-in role, in pipeline order.
    pub fn builtin() -> impl Iterator<Item = AgentRole> {
        BUILTIN_ROLES.iter().map(|(_, r)| r.clone())
    }

    /// Stable name used for prompts, configuration and logs.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            AgentRole::Custom(s) => s.clone(),
            other => BUILTIN_ROLES
                .iter()
                .find(|(_, r)| r == other)
                .map(|(n, _)| (*n).to_string())
                .unwrap_or_default(),
        }
    }

    /// Parse a role name; unknown names become [`AgentRole::Custom`].
    #[must_use]
    pub fn parse(name: &str) -> Self {
        BUILTIN_ROLES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| AgentRole::Custom(name.to_string()))
    }

    /// Whether this is one of the built-in roles.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        !matches!(self, AgentRole::Custom(_))
    }
}

impl serde::Serialize for AgentRole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.name())
    }
}

impl<'de> serde::Deserialize<'de> for AgentRole {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(AgentRole::parse(&s))
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

/// Which tools an agent may use.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelection {
    /// No tools at all (pure text generation).
    None,
    /// Read-only tools: `read_file`, `glob`, `grep`, `list_dir`.
    ReadOnly,
    /// Read-only tools plus `write_file` (for producing documents).
    ReadWrite,
    /// Every registered tool.
    #[default]
    All,
    /// An explicit list of tool names.
    Named(Vec<String>),
}

impl ToolSelection {
    /// Resolve the selection into a concrete list of names given the tools
    /// available in a registry.
    #[must_use]
    pub fn resolve<'a>(&self, available: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let available: Vec<&str> = available.into_iter().collect();
        match self {
            ToolSelection::None => vec![],
            ToolSelection::ReadOnly => available
                .iter()
                .filter(|n| READ_ONLY_TOOLS.contains(n))
                .map(|n| (*n).to_string())
                .collect(),
            ToolSelection::ReadWrite => available
                .iter()
                .filter(|n| READ_ONLY_TOOLS.contains(n) || **n == "write_file")
                .map(|n| (*n).to_string())
                .collect(),
            ToolSelection::All => available.iter().map(|n| (*n).to_string()).collect(),
            ToolSelection::Named(names) => names
                .iter()
                .filter(|n| available.contains(&n.as_str()))
                .cloned()
                .collect(),
        }
    }
}

/// Tools considered read-only by [`ToolSelection::ReadOnly`].
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "list_dir",
    "web_fetch",
    "web_search",
];

/// Reasoning effort, mapped to a provider-specific thinking budget.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// No extended thinking.
    Off,
    /// About 1k tokens.
    Low,
    /// About 4k tokens.
    #[default]
    Medium,
    /// About 16k tokens.
    High,
    /// About 32k tokens.
    Max,
}

impl ThinkingLevel {
    /// Token budget associated with the level.
    #[must_use]
    pub fn budget(self) -> Option<u32> {
        match self {
            ThinkingLevel::Off => None,
            ThinkingLevel::Low => Some(1024),
            ThinkingLevel::Medium => Some(4096),
            ThinkingLevel::High => Some(16384),
            ThinkingLevel::Max => Some(32768),
        }
    }
}

/// Which model an agent should use.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelection {
    /// Use the model configured for the current pipeline phase.
    #[default]
    Phase,
    /// Use a specific model.
    Fixed(ModelRef),
}

/// Declarative description of an agent.
///
/// An `AgentSpec` is data: it can be loaded from TOML, provided by a plugin,
/// or built in code. The agent runtime turns it into a conversation with a
/// [`crate::ModelProvider`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentSpec {
    /// Role of the agent.
    pub role: AgentRole,
    /// Human readable description.
    #[serde(default)]
    pub description: String,
    /// System prompt template (see [`crate::PromptTemplate`]).
    pub system_prompt: String,
    /// Tools the agent may use.
    #[serde(default)]
    pub tools: ToolSelection,
    /// Model to use.
    #[serde(default)]
    pub model: ModelSelection,
    /// Reasoning effort.
    #[serde(default)]
    pub thinking: ThinkingLevel,
    /// Maximum number of model round-trips.
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Whether the agent is expected to end with a JSON document.
    #[serde(default)]
    pub structured_output: bool,
    /// Output token budget per step.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_max_steps() -> u32 {
    200
}

fn default_max_tokens() -> u32 {
    8192
}

impl AgentSpec {
    /// Minimal spec with sensible defaults.
    pub fn new(role: AgentRole, system_prompt: impl Into<String>) -> Self {
        Self {
            role,
            description: String::new(),
            system_prompt: system_prompt.into(),
            tools: ToolSelection::All,
            model: ModelSelection::Phase,
            thinking: ThinkingLevel::Medium,
            max_steps: default_max_steps(),
            structured_output: false,
            max_tokens: default_max_tokens(),
        }
    }

    /// Set the tool selection.
    #[must_use]
    pub fn with_tools(mut self, tools: ToolSelection) -> Self {
        self.tools = tools;
        self
    }

    /// Set the thinking level.
    #[must_use]
    pub fn with_thinking(mut self, thinking: ThinkingLevel) -> Self {
        self.thinking = thinking;
        self
    }

    /// Set the step budget.
    #[must_use]
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Declare that the agent must end with JSON.
    #[must_use]
    pub fn with_structured_output(mut self) -> Self {
        self.structured_output = true;
        self
    }

    /// Set the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// Why an agent run ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum AgentStop {
    /// The model ended its turn without requesting tools.
    Completed,
    /// The step budget was exhausted.
    MaxSteps,
    /// The context window filled up.
    ContextWindow,
    /// A hook or the user cancelled the run.
    Cancelled,
    /// An unrecoverable error.
    Error {
        /// Error classification.
        kind: crate::ErrorKind,
        /// Message.
        message: String,
    },
}

/// Result of running an agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentOutcome {
    /// Role that ran.
    pub role: AgentRole,
    /// Why the run stopped.
    pub stop: AgentStop,
    /// Final assistant text (last text block(s)).
    pub final_text: String,
    /// Full transcript.
    pub messages: Vec<Message>,
    /// Number of model round-trips.
    pub steps: u32,
    /// Number of tool calls executed.
    pub tool_calls: u32,
    /// Accumulated token usage.
    pub usage: Usage,
}

impl AgentOutcome {
    /// Whether the run finished normally.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self.stop, AgentStop::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_roundtrip() {
        assert_eq!(AgentRole::QaReviewer.name(), "qa_reviewer");
        assert_eq!(AgentRole::parse("qa_reviewer"), AgentRole::QaReviewer);
        assert_eq!(
            AgentRole::parse("my_agent"),
            AgentRole::Custom("my_agent".into())
        );
        assert_eq!(AgentRole::Custom("x".into()).to_string(), "x");
        assert_eq!(
            serde_json::to_string(&AgentRole::Custom("x".into())).unwrap(),
            "\"x\""
        );
        assert_eq!(
            serde_json::to_string(&AgentRole::Coder).unwrap(),
            "\"coder\""
        );
        let back: AgentRole = serde_json::from_str("\"planner\"").unwrap();
        assert_eq!(back, AgentRole::Planner);
        assert_eq!(AgentRole::builtin().count(), 12);
    }

    #[test]
    fn tool_selection_resolves() {
        let avail = ["read_file", "write_file", "bash", "grep"];
        assert_eq!(ToolSelection::None.resolve(avail).len(), 0);
        assert_eq!(
            ToolSelection::ReadOnly.resolve(avail),
            vec!["read_file", "grep"]
        );
        assert_eq!(
            ToolSelection::ReadWrite.resolve(avail),
            vec!["read_file", "write_file", "grep"]
        );
        assert_eq!(ToolSelection::All.resolve(avail).len(), 4);
        assert_eq!(
            ToolSelection::Named(vec!["bash".into(), "nope".into()]).resolve(avail),
            vec!["bash"]
        );
    }

    #[test]
    fn thinking_budgets() {
        assert_eq!(ThinkingLevel::Off.budget(), None);
        assert_eq!(ThinkingLevel::High.budget(), Some(16384));
    }
}
