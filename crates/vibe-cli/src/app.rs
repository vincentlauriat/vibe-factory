//! Wiring: turns the configuration of a project into the registry, providers,
//! workspace, plugins, store and event bus the pipeline needs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use vibe_core::config::{CONFIG_FILE, PluginConfig, VIBE_DIR};
use vibe_core::{
    EventBus, MergeStrategy as ConfigMergeStrategy, ModelProvider, ModelRef, Phase, Registry,
    SharedWorkspaceProvider, Task, ToolRegistry, VibeConfig, Workspace, WorkspaceKind,
};
use vibe_pipeline::{Committer, FileTaskStore, Pipeline, PipelineDeps, PipelineStore, Resetter};
use vibe_plugins::PluginHost;
use vibe_providers::ProviderRegistry;
use vibe_workspace::{
    ContainerRunner, ContainerSettings, ContainerWorkspace, GitWorktreeProvider,
    MergeStrategy as WorkspaceMergeStrategy,
};

use crate::mock::{RoleMatcher, Script, mock_provider};

/// Name of the provider the CLI registers for mock runs.
pub const MOCK_PROVIDER: &str = "mock";

/// Per-invocation changes to the configuration.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// Use this provider for every phase.
    pub provider: Option<String>,
    /// Use this `provider/model` for every phase.
    pub model: Option<String>,
    /// Workspace provider name.
    pub workspace: Option<String>,
    /// Merge automatically after QA approval.
    pub auto_merge: bool,
    /// Scripted responses for the mock provider.
    pub script: Option<PathBuf>,
    /// Token budget of the run.
    pub max_tokens: Option<u64>,
    /// Active time budget of the run, in seconds.
    pub max_duration_secs: Option<u64>,
}

/// Path of the configuration file of a project.
pub fn config_path(root: &Path) -> PathBuf {
    root.join(VIBE_DIR).join(CONFIG_FILE)
}

/// Load the configuration of a project (defaults when absent).
pub fn load_config(root: &Path) -> Result<VibeConfig> {
    VibeConfig::load(root).with_context(|| format!("cannot load {}", config_path(root).display()))
}

/// Open the task store of a project.
pub fn open_store(root: &Path) -> Result<Arc<FileTaskStore>> {
    Ok(Arc::new(FileTaskStore::open(root).with_context(|| {
        format!("cannot open the task store of {}", root.display())
    })?))
}

/// Built-in agents with the project's `.vibe/agents` overrides applied.
pub fn agent_registry(root: &Path) -> Result<Registry> {
    let mut registry = Registry::new();
    vibe_agents::register_builtin_agents(&mut registry);
    let dir = vibe_agents::project_agents_dir(root);
    vibe_agents::apply_agent_overrides(&mut registry, &dir)
        .with_context(|| format!("invalid agent override in {}", dir.display()))?;
    Ok(registry)
}

/// Plugin declarations of a project (inline + discovered), each running in
/// the project root unless it says otherwise.
pub fn plugin_configs(root: &Path, config: &VibeConfig) -> Vec<PluginConfig> {
    let mut configs = vibe_plugins::discovery::plugin_configs(root, config);
    for c in &mut configs {
        if c.cwd.is_none() {
            c.cwd = Some(root.to_path_buf());
        }
    }
    configs
}

/// Apply `--provider` / `--model`: every phase then uses that provider or
/// model (per-phase models are replaced, thinking levels are kept).
pub fn apply_model_overrides(config: &mut VibeConfig, provider: Option<&str>, model: Option<&str>) {
    if let Some(p) = provider {
        config.default_provider = p.to_string();
        config.default_model = format!("{p}/");
    }
    if let Some(m) = model {
        config.default_model = ModelRef::parse(m, &config.default_provider).to_string();
    }
    if provider.is_some() || model.is_some() {
        for pm in config.phases.phases.values_mut() {
            pm.model.clone_from(&config.default_model);
        }
    }
}

/// Names of the providers the configuration actually uses (default provider
/// and every phase model).
pub fn referenced_providers(config: &VibeConfig) -> Vec<String> {
    let mut names: Vec<String> = Phase::ALL
        .into_iter()
        .map(|p| config.model_for(p).0.provider)
        .collect();
    names.push(config.default_provider.clone());
    names.sort();
    names.dedup();
    names
}

/// Resolves models against the configured providers first, then against
/// providers contributed by plugins.
#[derive(Clone)]
pub struct CliResolver {
    providers: ProviderRegistry,
    registry: Arc<Registry>,
}

impl vibe_pipeline::ProviderResolver for CliResolver {
    fn resolve(&self, model: &ModelRef) -> vibe_core::Result<(Arc<dyn ModelProvider>, String)> {
        match self.providers.resolve(model) {
            Ok(found) => Ok(found),
            Err(err) => match self.registry.provider(&model.provider) {
                Some(p) => {
                    let id = if model.model.trim().is_empty() {
                        p.info().default_model
                    } else {
                        model.model.clone()
                    };
                    Ok((Arc::clone(p), id))
                }
                None => Err(err),
            },
        }
    }
}

/// Everything a pipeline run needs.
pub struct AppContext {
    /// Project root.
    pub root: PathBuf,
    /// Effective configuration (with overrides).
    pub config: VibeConfig,
    /// Agents, tools, providers and plugin contributions.
    pub registry: Arc<Registry>,
    /// Model resolution.
    pub resolver: CliResolver,
    /// Workspace provider.
    pub workspace: SharedWorkspaceProvider,
    /// Task store.
    pub store: Arc<FileTaskStore>,
    /// Event bus.
    pub events: EventBus,
    /// Git committer, when the project is a git repository.
    pub committer: Option<Committer>,
    /// Discards the uncommitted changes of a failed subtask attempt, when
    /// the project is a git repository.
    pub resetter: Option<Resetter>,
    /// Worktree per subtask attempt, with the `git_worktree` workspace.
    pub subtask_workspaces: Option<Arc<dyn vibe_core::SubtaskWorkspaces>>,
    /// Running plugins (shut down by [`AppContext::shutdown`]).
    pub plugins: PluginHost,
    /// Whether the project is a git repository.
    pub is_git: bool,
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("root", &self.root)
            .field("workspace", &self.workspace.name())
            .field("is_git", &self.is_git)
            .finish_non_exhaustive()
    }
}

/// Whether `root` is inside a git work tree.
pub async fn is_git_repo(root: &Path) -> bool {
    vibe_workspace::Git::new(root).is_repo().await
}

/// Build the whole context of a run. See the crate documentation for the
/// order of the steps.
pub async fn build_context(root: &Path, overrides: &Overrides) -> Result<AppContext> {
    let mut config = load_config(root)?;
    let script = match &overrides.script {
        Some(path) => Some(Script::load(path)?),
        None => None,
    };
    let provider_override = if script.is_some() {
        Some(MOCK_PROVIDER)
    } else {
        overrides.provider.as_deref()
    };
    let model_override = if script.is_some() {
        None
    } else {
        overrides.model.as_deref()
    };
    apply_model_overrides(&mut config, provider_override, model_override);
    if let Some(ws) = &overrides.workspace {
        config.pipeline.workspace.clone_from(ws);
    }
    if overrides.auto_merge {
        config.pipeline.auto_merge = true;
    }
    if let Some(t) = overrides.max_tokens {
        config.pipeline.max_tokens = Some(t);
    }
    if let Some(s) = overrides.max_duration_secs {
        config.pipeline.max_duration_secs = Some(s);
    }

    // Container settings are validated before anything starts.
    let container = container_settings(root, &config)?;

    // Agents and tools. With the container workspace, `bash` (and therefore
    // the required validation commands) runs in containers.
    let mut registry = agent_registry(root)?;
    registry
        .tools
        .extend(&vibe_tools::builtin_tools(&config.security));
    if let Some(settings) = &container {
        let runner = Arc::new(ContainerRunner::new(settings.clone()));
        registry.tools.register(Arc::new(
            vibe_tools::BashTool::new(&config.security).with_runner(runner),
        ));
    }

    // Providers. The CLI's mock is used for `--script`, and for `mock` when
    // the configuration does not declare one.
    let mut providers =
        ProviderRegistry::from_config(&config).context("invalid provider configuration")?;
    let wants_mock = referenced_providers(&config)
        .iter()
        .any(|p| p == MOCK_PROVIDER);
    if script.is_some() || (wants_mock && providers.get(MOCK_PROVIDER).is_none()) {
        let matcher = RoleMatcher::from_agents(registry.agents.values());
        providers.register_with_kind(
            MOCK_PROVIDER,
            "mock",
            Arc::new(mock_provider(script, matcher)),
        );
    }
    providers.install_into(&mut registry);

    // Plugins.
    let configs = plugin_configs(root, &config);
    let plugins = PluginHost::load(&configs)
        .await
        .context("cannot start plugins")?;
    plugins
        .register_all(&mut registry)
        .context("cannot register plugins")?;
    let registry = Arc::new(registry);
    let resolver = CliResolver {
        providers,
        registry: Arc::clone(&registry),
    };

    // Workspace.
    let is_git = is_git_repo(root).await;
    let workspace = workspace_provider(&config, &registry, &resolver, container)?;
    if uses_worktrees(workspace.name()) && !is_git {
        anyhow::bail!(
            "{} is not a git repository: run `git init`, or use `--workspace in_place`",
            root.display()
        );
    }

    let store = open_store(root)?;
    // The pipeline routes every run's events to the store's event log
    // itself; adding a `FileEventSink` here would log them twice.
    let events = EventBus::default();
    let committer = is_git.then(git_committer);
    let resetter = is_git.then(git_resetter);
    let subtask_workspaces: Option<Arc<dyn vibe_core::SubtaskWorkspaces>> = (is_git
        && uses_worktrees(workspace.name()))
    .then(|| -> Result<Arc<dyn vibe_core::SubtaskWorkspaces>> {
        Ok(Arc::new(
            vibe_workspace::GitSubtaskWorkspaces::new()
                .with_merge_strategy(merge_strategy(&config, &resolver)?),
        ))
    })
    .transpose()?;

    Ok(AppContext {
        root: root.to_path_buf(),
        config,
        registry,
        resolver,
        workspace,
        store,
        events,
        committer,
        resetter,
        subtask_workspaces,
        plugins,
        is_git,
    })
}

/// Whether the workspace provider `name` keeps a git worktree per task
/// under `.vibe/worktrees` (`git_worktree` and `container`).
pub fn uses_worktrees(name: &str) -> bool {
    matches!(
        name,
        "git_worktree" | vibe_workspace::container::PROVIDER_NAME
    )
}

/// Validated `[workspace.container]` settings when the configured workspace
/// provider is `container` (`None` for any other provider).
pub fn container_settings(root: &Path, config: &VibeConfig) -> Result<Option<ContainerSettings>> {
    if config.pipeline.workspace != vibe_workspace::container::PROVIDER_NAME {
        return Ok(None);
    }
    let Some(container) = &config.workspace.container else {
        anyhow::bail!(
            "the `container` workspace needs a [workspace.container] table with at least \
             `image = \"…\"` in {}",
            config_path(root).display()
        );
    };
    ContainerSettings::from_config(container, root)
        .map(Some)
        .context("invalid [workspace.container] configuration")
}

/// How merge and subtask integration conflicts are handled: assisted
/// resolutions use the model of the merge phase, metered against the run
/// budget.
fn merge_strategy(config: &VibeConfig, resolver: &CliResolver) -> Result<WorkspaceMergeStrategy> {
    if config.pipeline.merge_strategy != ConfigMergeStrategy::Assisted {
        return Ok(WorkspaceMergeStrategy::Manual);
    }
    let (model, _) = config.model_for(Phase::Merge);
    let (llm, model) = vibe_pipeline::ProviderResolver::resolve(resolver, &model)
        .context("cannot resolve the model of assisted merges")?;
    Ok(WorkspaceMergeStrategy::Assisted {
        provider: Arc::new(vibe_core::MeteredProvider::new(llm)),
        model,
    })
}

fn workspace_provider(
    config: &VibeConfig,
    registry: &Registry,
    resolver: &CliResolver,
    container: Option<ContainerSettings>,
) -> Result<SharedWorkspaceProvider> {
    let name = config.pipeline.workspace.as_str();
    if uses_worktrees(name) {
        let provider = GitWorktreeProvider::new()
            .with_base_branch(config.base_branch.clone())
            .with_merge_strategy(merge_strategy(config, resolver)?);
        return Ok(match container {
            Some(settings) => Arc::new(ContainerWorkspace::new(provider, settings)),
            None => Arc::new(provider),
        });
    }
    if let Some(p) = vibe_workspace::provider_by_name(name, config.base_branch.clone()) {
        return Ok(p);
    }
    registry.workspaces.get(name).cloned().ok_or_else(|| {
        let mut known = vec![
            "git_worktree".to_string(),
            "in_place".to_string(),
            "container".to_string(),
        ];
        known.extend(registry.workspaces.keys().cloned());
        anyhow!(
            "unknown workspace provider `{name}` (known: {})",
            known.join(", ")
        )
    })
}

/// Committer backed by `git add -A && git commit` in the workspace.
pub fn git_committer() -> Committer {
    Arc::new(|root: PathBuf, message: String| {
        Box::pin(async move { vibe_workspace::commit_all(&root, &message, &[]).await })
    })
}

/// Resetter backed by `git checkout -- . && git clean -fd -e .vibe` in the
/// workspace: a failed subtask attempt never leaks into the next one.
pub fn git_resetter() -> Resetter {
    Arc::new(|root: PathBuf| {
        Box::pin(async move {
            let git = vibe_workspace::Git::new(&root);
            git.run(&["checkout", "--", "."]).await?;
            git.run(&["clean", "-fd", "-e", ".vibe"]).await?;
            Ok(())
        })
    })
}

impl AppContext {
    /// A pipeline over this context.
    pub fn pipeline(&self) -> Pipeline {
        let store: Arc<dyn PipelineStore> = self.store.clone();
        Pipeline::new(PipelineDeps {
            registry: Arc::clone(&self.registry),
            providers: Arc::new(self.resolver.clone()),
            store,
            workspace: Arc::clone(&self.workspace),
            // Built-in tools are already in the registry.
            tools: ToolRegistry::new(),
            events: self.events.clone(),
            config: self.config.clone(),
            project_root: self.root.clone(),
            committer: self.committer.clone(),
            resetter: self.resetter.clone(),
            subtask_workspaces: self.subtask_workspaces.clone(),
        })
    }

    /// Stop every plugin process.
    pub async fn shutdown(mut self) {
        self.plugins.shutdown_all().await;
    }
}

/// Location of a task's git worktree, computed without creating anything.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorktreeLocation {
    /// Branch name.
    pub branch: String,
    /// Worktree directory.
    pub path: PathBuf,
}

/// Where the git worktree of `task` lives (whether or not it exists).
pub fn worktree_location(root: &Path, task: &Task) -> WorktreeLocation {
    let provider = GitWorktreeProvider::new();
    WorktreeLocation {
        branch: GitWorktreeProvider::task_branch(task),
        path: provider
            .worktrees_root(root)
            .join(GitWorktreeProvider::task_name(task)),
    }
}

/// The [`Workspace`] value of a task's git worktree, for `discard`.
pub fn worktree_workspace(root: &Path, task: &Task) -> Workspace {
    let loc = worktree_location(root, task);
    Workspace {
        root: loc.path,
        project_root: root.to_path_buf(),
        kind: WorkspaceKind::GitWorktree,
        branch: Some(loc.branch),
        base_branch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_override_rewrites_every_phase() {
        let mut cfg =
            VibeConfig::from_toml("[phases.plan]\nmodel = \"openai/gpt-5\"\nthinking = \"high\"\n")
                .unwrap();
        apply_model_overrides(&mut cfg, Some("mock"), None);
        for p in Phase::ALL {
            let (m, _) = cfg.model_for(p);
            assert_eq!(m, ModelRef::new("mock", ""), "{p}");
        }
        assert_eq!(
            cfg.model_for(Phase::Plan).1,
            Some(vibe_core::ThinkingLevel::High)
        );
        assert_eq!(referenced_providers(&cfg), vec!["mock".to_string()]);
    }

    #[test]
    fn model_override_uses_default_provider_for_bare_names() {
        let mut cfg = VibeConfig::default();
        apply_model_overrides(&mut cfg, None, Some("opus"));
        assert_eq!(cfg.default_model, "anthropic/opus");
        apply_model_overrides(&mut cfg, Some("openai"), Some("gpt-5"));
        assert_eq!(cfg.default_model, "openai/gpt-5");
    }

    #[test]
    fn worktree_location_is_under_vibe() {
        let t = Task::new("Add login", "");
        let loc = worktree_location(Path::new("/p"), &t);
        assert!(loc.branch.starts_with("vibe/add-login-"));
        assert!(
            loc.path
                .starts_with(Path::new("/p").join(".vibe").join("worktrees"))
        );
    }
}
