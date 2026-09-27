//! Project and user configuration (`.vibe/config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agent::ThinkingLevel;
use crate::error::Result;
use crate::phase::Phase;
use crate::provider::ModelRef;
pub use crate::sandbox::{ContainerConfig, ContainerMount, WorkspaceConfig};

/// Name of the directory holding framework data inside a project.
pub const VIBE_DIR: &str = ".vibe";

/// Name of the configuration file inside [`VIBE_DIR`].
pub const CONFIG_FILE: &str = "config.toml";

/// Configuration of one model provider.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig {
    /// Provider kind: `anthropic`, `openai`, `ollama`, or a plugin name.
    pub kind: String,
    /// API key. Prefer `api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable holding the API key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Base URL override (self-hosted gateways, Ollama, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default model for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Context window of this provider's models, in tokens (default 200 000).
    /// Used to warn agents near the limit and stop before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Extra provider-specific settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
}

impl ProviderConfig {
    /// Resolve the API key from the inline value or the environment.
    #[must_use]
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            return Some(k.clone());
        }
        self.api_key_env
            .as_ref()
            .and_then(|v| std::env::var(v).ok())
            .filter(|k| !k.is_empty())
    }
}

/// Model and thinking level for one phase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhaseModel {
    /// `provider/model` or bare model on the default provider.
    pub model: String,
    /// Thinking level override. When absent every agent of the phase keeps
    /// the thinking level of its own [`crate::AgentSpec`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
}

/// Model assignment per phase.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseModels {
    /// Overrides per phase (missing phases use `default`).
    #[serde(flatten)]
    pub phases: BTreeMap<String, PhaseModel>,
}

impl PhaseModels {
    /// Look up the override for a phase.
    #[must_use]
    pub fn get(&self, phase: Phase) -> Option<&PhaseModel> {
        self.phases.get(phase.as_str())
    }
}

/// Pipeline tuning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PipelineConfig {
    /// Maximum QA review/fix rounds.
    #[serde(default = "default_qa_rounds")]
    pub max_qa_rounds: u32,
    /// Attempts per subtask before it is marked failed.
    #[serde(default = "default_subtask_attempts")]
    pub max_subtask_attempts: u32,
    /// Maximum subtasks implemented concurrently.
    #[serde(default = "default_parallel")]
    pub max_parallel_subtasks: usize,
    /// Retries per phase when the agent produced no usable output.
    #[serde(default = "default_phase_retries")]
    pub max_phase_retries: u32,
    /// Workspace provider name (`git_worktree`, `in_place`, `container`, or
    /// a plugin).
    #[serde(default = "default_workspace")]
    pub workspace: String,
    /// Whether to merge automatically after QA approval.
    #[serde(default)]
    pub auto_merge: bool,
    /// Required shell checks, run in order before marking ready or merging.
    #[serde(default)]
    pub validation_commands: Vec<String>,
    /// Maximum automatic fixes of required validation failures over a whole run.
    /// Zero disables automatic validation fixes. Resumes retain the consumed budget.
    #[serde(default = "default_validation_fix_attempts")]
    pub max_validation_fix_attempts: u32,
    /// How merge conflicts are handled: `manual` (report for a human) or
    /// `assisted` (let the `merge_resolver` agent's model try first).
    #[serde(default)]
    pub merge_strategy: MergeStrategy,
    /// Give every subtask attempt its own workspace when the workspace
    /// provider supports it (`git_worktree`), and integrate finished
    /// subtasks one at a time. When false, parallel subtasks share the task
    /// workspace.
    #[serde(default = "default_true")]
    pub isolate_subtasks: bool,
    /// Maximum input plus output tokens of a run, counted across resumes.
    /// When reached the run pauses; raise the limit and resume to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Maximum active time of a run in seconds, counted across resumes
    /// (time spent paused is not counted). When reached the run pauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_secs: Option<u64>,
}

impl PipelineConfig {
    /// Budget limits of a run.
    #[must_use]
    pub fn budget_limits(&self) -> crate::BudgetLimits {
        crate::BudgetLimits {
            max_tokens: self.max_tokens,
            max_duration: self.max_duration_secs.map(std::time::Duration::from_secs),
        }
    }
}

/// Conflict handling strategy at merge time.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Conflicts are reported for a human to resolve.
    #[default]
    Manual,
    /// A model attempts to resolve conflicts; unresolved files go to a human.
    Assisted,
}

fn default_true() -> bool {
    true
}

fn default_validation_fix_attempts() -> u32 {
    2
}

fn default_qa_rounds() -> u32 {
    3
}
fn default_subtask_attempts() -> u32 {
    3
}
fn default_parallel() -> usize {
    3
}
fn default_phase_retries() -> u32 {
    2
}
fn default_workspace() -> String {
    "git_worktree".to_string()
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_qa_rounds: default_qa_rounds(),
            max_subtask_attempts: default_subtask_attempts(),
            max_parallel_subtasks: default_parallel(),
            max_phase_retries: default_phase_retries(),
            workspace: default_workspace(),
            auto_merge: false,
            validation_commands: Vec::new(),
            max_validation_fix_attempts: default_validation_fix_attempts(),
            merge_strategy: MergeStrategy::Manual,
            isolate_subtasks: true,
            max_tokens: None,
            max_duration_secs: None,
        }
    }
}

/// Security policy for tools.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecurityConfig {
    /// Additional programs that must never run.
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    /// If non-empty, only these programs may run.
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    /// Default shell command timeout in seconds.
    #[serde(default = "default_timeout")]
    pub command_timeout_secs: u64,
    /// Whether tools may access the network.
    #[serde(default)]
    pub allow_network: bool,
    /// Paths outside the project agents may read.
    #[serde(default)]
    pub extra_read_paths: Vec<PathBuf>,
}

fn default_timeout() -> u64 {
    120
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            blocked_commands: Vec::new(),
            allowed_commands: Vec::new(),
            command_timeout_secs: default_timeout(),
            allow_network: false,
            extra_read_paths: Vec::new(),
        }
    }
}

/// Declaration of a plugin to load.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    /// Plugin name (for logs and `vibe plugins list`).
    pub name: String,
    /// Executable (with arguments) speaking the plugin protocol over stdio.
    #[serde(default)]
    pub command: Vec<String>,
    /// Environment variables for the process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory of the process. The CLI fills it with the project
    /// root when absent; the raw plugin client falls back to the host's
    /// current directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Capabilities the plugin claims to offer (`tools`, `agents`, `hooks`);
    /// informational, the handshake is authoritative.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Whether a failure to start is fatal.
    #[serde(default)]
    pub required: bool,
}

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VibeConfig {
    /// Name of the provider used when a model has no `provider/` prefix.
    #[serde(default = "default_provider")]
    pub default_provider: String,
    /// Default `provider/model` for every phase.
    #[serde(default = "default_model")]
    pub default_model: String,
    /// Per-phase overrides.
    #[serde(default)]
    pub phases: PhaseModels,
    /// Providers by name. When the table is absent the built-in
    /// `anthropic`, `openai` and `ollama` entries are used.
    #[serde(default = "default_providers")]
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Pipeline tuning.
    #[serde(default)]
    pub pipeline: PipelineConfig,
    /// Security policy.
    #[serde(default)]
    pub security: SecurityConfig,
    /// Plugins to load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginConfig>,
    /// Base branch for worktrees (default: current branch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Workspace provider settings (`[workspace.container]`).
    #[serde(default, skip_serializing_if = "WorkspaceConfig::is_empty")]
    pub workspace: WorkspaceConfig,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "anthropic/claude-sonnet-5".to_string()
}

/// Built-in provider table used when the configuration declares none.
#[must_use]
pub fn default_providers() -> BTreeMap<String, ProviderConfig> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderConfig {
            kind: "anthropic".into(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            ..Default::default()
        },
    );
    providers.insert(
        "openai".to_string(),
        ProviderConfig {
            kind: "openai".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            ..Default::default()
        },
    );
    providers.insert(
        "ollama".to_string(),
        ProviderConfig {
            kind: "openai".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            default_model: Some("qwen2.5-coder".into()),
            ..Default::default()
        },
    );
    providers
}

impl Default for VibeConfig {
    fn default() -> Self {
        Self {
            default_provider: default_provider(),
            default_model: default_model(),
            phases: PhaseModels::default(),
            providers: default_providers(),
            pipeline: PipelineConfig::default(),
            security: SecurityConfig::default(),
            plugins: Vec::new(),
            base_branch: None,
            workspace: WorkspaceConfig::default(),
        }
    }
}

impl VibeConfig {
    /// Parse from TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Serialise to TOML text.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| crate::Error::config(format!("cannot serialise config: {e}")))
    }

    /// Load `<project>/.vibe/config.toml`, or defaults when absent.
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(VIBE_DIR).join(CONFIG_FILE);
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            Self::from_toml(&text)
        } else {
            Ok(Self::default())
        }
    }

    /// Write `<project>/.vibe/config.toml`.
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let dir = project_root.join(VIBE_DIR);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(CONFIG_FILE), self.to_toml()?)?;
        Ok(())
    }

    /// Model to use for a phase, and the thinking level override if the
    /// phase declares one (`None` means "keep each agent's own level").
    #[must_use]
    pub fn model_for(&self, phase: Phase) -> (ModelRef, Option<ThinkingLevel>) {
        match self.phases.get(phase) {
            Some(pm) => (
                ModelRef::parse(&pm.model, &self.default_provider),
                pm.thinking,
            ),
            None => (
                ModelRef::parse(&self.default_model, &self.default_provider),
                None,
            ),
        }
    }

    /// Provider configuration by name.
    #[must_use]
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_through_toml() {
        let cfg = VibeConfig::default();
        let text = cfg.to_toml().unwrap();
        let back = VibeConfig::from_toml(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn phase_override() {
        let text = r#"
default_model = "anthropic/claude-sonnet-5"
[phases.plan]
model = "openai/gpt-5"
thinking = "high"
"#;
        let cfg = VibeConfig::from_toml(text).unwrap();
        let (m, t) = cfg.model_for(Phase::Plan);
        assert_eq!(m, ModelRef::new("openai", "gpt-5"));
        assert_eq!(t, Some(ThinkingLevel::High));
        let (m, t) = cfg.model_for(Phase::Build);
        assert_eq!(m.provider, "anthropic");
        assert_eq!(t, None);
    }

    #[test]
    fn missing_providers_table_uses_builtins() {
        let cfg = VibeConfig::from_toml("default_model = \"anthropic/sonnet\"").unwrap();
        assert!(cfg.providers.contains_key("anthropic"));
        assert!(cfg.providers.contains_key("ollama"));
        assert_eq!(cfg.pipeline.merge_strategy, MergeStrategy::Manual);
    }

    #[test]
    fn load_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VibeConfig::load(dir.path()).unwrap();
        cfg.save(dir.path()).unwrap();
        assert!(dir.path().join(".vibe/config.toml").exists());
        assert_eq!(VibeConfig::load(dir.path()).unwrap(), cfg);
    }
}
