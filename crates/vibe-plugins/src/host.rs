//! Loading and managing every plugin of a run.

use std::collections::BTreeSet;

use vibe_core::config::PluginConfig;
use vibe_core::{Error, Plugin, Registry, Result};

use crate::protocol::{Capabilities, HostInfo};
use crate::remote::RemotePlugin;

/// Summary of a loaded plugin, for listings such as `vibe plugins list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPlugin {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// What the plugin contributes.
    pub capabilities: Capabilities,
    /// Names of the tools it contributes.
    pub tool_names: Vec<String>,
    /// Whether it runs in-process.
    pub native: bool,
}

/// Owns every plugin of a run: out-of-process plugins started from
/// [`PluginConfig`]s and in-process [`Plugin`] implementations.
pub struct PluginHost {
    host: HostInfo,
    remotes: Vec<RemotePlugin>,
    natives: Vec<Box<dyn Plugin>>,
    loaded: Vec<LoadedPlugin>,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("host", &self.host)
            .field("plugins", &self.loaded)
            .finish()
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    /// Host without plugins, announcing itself as [`HostInfo::vibe_factory`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_host_info(HostInfo::vibe_factory())
    }

    /// Host without plugins, announcing itself as `host`.
    #[must_use]
    pub fn with_host_info(host: HostInfo) -> Self {
        Self {
            host,
            remotes: Vec::new(),
            natives: Vec::new(),
            loaded: Vec::new(),
        }
    }

    /// Start every configured plugin concurrently.
    ///
    /// A plugin that fails to start is logged and skipped, unless it is
    /// `required`: then every plugin already started is shut down and the
    /// error is returned. Duplicate names are ignored after the first.
    pub async fn load(configs: &[PluginConfig]) -> Result<Self> {
        let mut host = Self::new();
        host.load_all(configs).await?;
        Ok(host)
    }

    /// Start the given plugins and add them to this host (see
    /// [`load`](Self::load) for the failure policy).
    pub async fn load_all(&mut self, configs: &[PluginConfig]) -> Result<()> {
        let mut names: BTreeSet<String> = self.loaded.iter().map(|p| p.name.clone()).collect();
        let mut unique = Vec::new();
        for config in configs {
            if names.insert(config.name.clone()) {
                unique.push(config);
            } else {
                tracing::warn!(
                    "plugin `{}` declared twice; ignoring duplicate",
                    config.name
                );
            }
        }
        let started = futures::future::join_all(
            unique
                .iter()
                .map(|c| RemotePlugin::start(c, self.host.clone())),
        )
        .await;

        let mut fatal = None;
        for (config, outcome) in unique.iter().zip(started) {
            match outcome {
                Ok(plugin) => self.add_remote(plugin),
                Err(e) if config.required => {
                    tracing::error!("required plugin `{}` failed to start: {e}", config.name);
                    fatal.get_or_insert(Error::plugin(format!(
                        "required plugin `{}` failed to start: {}",
                        config.name, e.message
                    )));
                }
                Err(e) => tracing::warn!("skipping plugin `{}`: {e}", config.name),
            }
        }
        match fatal {
            Some(err) => {
                self.shutdown_all().await;
                Err(err)
            }
            None => Ok(()),
        }
    }

    /// Add an already connected out-of-process plugin.
    pub fn add_remote(&mut self, plugin: RemotePlugin) {
        self.loaded.push(LoadedPlugin {
            name: plugin.name().to_string(),
            version: plugin.version().to_string(),
            capabilities: plugin.capabilities(),
            tool_names: plugin.tools().iter().map(|t| t.name.clone()).collect(),
            native: false,
        });
        self.remotes.push(plugin);
    }

    /// Add an in-process plugin. Its contributions are probed once, by
    /// registering it into a scratch registry, to fill [`LoadedPlugin`].
    pub fn add_native(&mut self, plugin: Box<dyn Plugin>) {
        let mut probe = Registry::new();
        let (capabilities, tool_names) = match plugin.register(&mut probe) {
            Ok(()) => (
                Capabilities {
                    tools: !probe.tools.is_empty(),
                    agents: !probe.agents.is_empty(),
                    hooks: !probe.hooks.is_empty(),
                },
                probe.tools.names().map(String::from).collect(),
            ),
            Err(e) => {
                tracing::warn!("native plugin `{}` failed to register: {e}", plugin.name());
                (Capabilities::default(), Vec::new())
            }
        };
        self.loaded.push(LoadedPlugin {
            name: plugin.name().to_string(),
            version: plugin.version().to_string(),
            capabilities,
            tool_names,
            native: true,
        });
        self.natives.push(plugin);
    }

    /// Every loaded plugin, remote and native, in load order.
    #[must_use]
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.loaded
    }

    /// The out-of-process plugins.
    #[must_use]
    pub fn remotes(&self) -> &[RemotePlugin] {
        &self.remotes
    }

    /// Register every plugin's contributions: native plugins first, then
    /// remote plugins, each in load order (later registrations override
    /// earlier ones of the same name).
    pub fn register_all(&self, registry: &mut Registry) -> Result<()> {
        for plugin in &self.natives {
            plugin.register(registry)?;
        }
        for plugin in &self.remotes {
            plugin.register(registry)?;
        }
        Ok(())
    }

    /// Stop every out-of-process plugin concurrently.
    pub async fn shutdown_all(&mut self) {
        futures::future::join_all(self.remotes.iter().map(RemotePlugin::shutdown)).await;
        self.remotes.clear();
        self.loaded.retain(|p| p.native);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vibe_core::{AgentRole, AgentSpec, Tool, ToolContext, ToolOutput};

    struct Native;
    struct NoopTool;

    #[async_trait::async_trait]
    impl Tool for NoopTool {
        fn name(&self) -> &str {
            "noop"
        }
        fn description(&self) -> &str {
            "does nothing"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _: &ToolContext, _: serde_json::Value) -> Result<ToolOutput> {
            Ok(ToolOutput::ok(""))
        }
    }

    impl Plugin for Native {
        fn name(&self) -> &str {
            "native"
        }
        fn version(&self) -> &str {
            "1.0.0"
        }
        fn register(&self, registry: &mut Registry) -> Result<()> {
            registry.add_tool(Arc::new(NoopTool));
            registry.add_agent(AgentSpec::new(AgentRole::Custom("n".into()), "p"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn native_plugins_are_listed_and_registered() {
        let mut host = PluginHost::default();
        host.add_native(Box::new(Native));
        let info = &host.plugins()[0];
        assert!(info.native);
        assert_eq!(info.tool_names, vec!["noop"]);
        assert!(info.capabilities.tools && info.capabilities.agents && !info.capabilities.hooks);

        let mut reg = Registry::new();
        host.register_all(&mut reg).unwrap();
        assert!(reg.tools.get("noop").is_some());
        host.shutdown_all().await;
        assert_eq!(host.plugins().len(), 1);
    }

    fn broken(name: &str, required: bool) -> PluginConfig {
        PluginConfig {
            name: name.into(),
            command: vec!["vibe-plugin-that-does-not-exist-anywhere".into()],
            env: Default::default(),
            cwd: None,
            capabilities: Vec::new(),
            required,
        }
    }

    #[tokio::test]
    async fn optional_failures_are_skipped() {
        let host = PluginHost::load(&[broken("a", false)]).await.unwrap();
        assert!(host.plugins().is_empty());
    }

    #[tokio::test]
    async fn required_failures_are_fatal() {
        let err = PluginHost::load(&[broken("a", false), broken("b", true)])
            .await
            .unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Plugin);
        assert!(err.message.contains("`b`"));
    }
}
