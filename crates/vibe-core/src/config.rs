//! Project and user configuration (`.vibe/config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agent::ThinkingLevel;
use crate::error::Result;
use crate::phase::Phase;
use crate::provider::ModelRef;

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
    /// Thinking level.
    #[serde(default)]
    pub thinking: ThinkingLevel,
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
    /// Workspace provider name (`git_worktree`, `in_place`, or a plugin).
    #[serde(default = "default_workspace")]
    pub workspace: String,
    /// Whether to merge automatically after QA approval.
    #[serde(default)]
    pub auto_merge: bool,
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
    /// Default thinking level.
    #[serde(default)]
    pub default_thinking: ThinkingLevel,
    /// Per-phase overrides.
    #[serde(default)]
    pub phases: PhaseModels,
    /// Providers by name.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Pipeline tuning.
    #[serde(default)]
    pub pipeline: PipelineConfig,
    /// Security policy.
    #[serde(default)]
    pub security: SecurityConfig,
    /// Plugins to load.
    #[serde(default)]
    pub plugins: Vec<PluginConfig>,
    /// Base branch for worktrees (default: current branch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "anthropic/claude-sonnet-5".to_string()
}

impl Default for VibeConfig {
    fn default() -> Self {
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
        Self {
            default_provider: default_provider(),
            default_model: default_model(),
            default_thinking: ThinkingLevel::Medium,
            phases: PhaseModels::default(),
            providers,
            pipeline: PipelineConfig::default(),
            security: SecurityConfig::default(),
            plugins: Vec::new(),
            base_branch: None,
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

    /// Model and thinking level to use for a phase.
    #[must_use]
    pub fn model_for(&self, phase: Phase) -> (ModelRef, ThinkingLevel) {
        match self.phases.get(phase) {
            Some(pm) => (
                ModelRef::parse(&pm.model, &self.default_provider),
                pm.thinking,
            ),
            None => (
                ModelRef::parse(&self.default_model, &self.default_provider),
                self.default_thinking,
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
        assert_eq!(t, ThinkingLevel::High);
        let (m, _) = cfg.model_for(Phase::Build);
        assert_eq!(m.provider, "anthropic");
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
