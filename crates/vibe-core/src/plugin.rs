//! Plugins and hooks: how the framework is extended.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::agent::{AgentRole, AgentSpec};
use crate::error::Result;
use crate::memory::SharedMemory;
use crate::phase::Phase;
use crate::provider::SharedProvider;
use crate::tool::{SharedTool, ToolContext, ToolRegistry};
use crate::workspace::SharedWorkspaceProvider;

/// Decision returned by a hook.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HookDecision {
    /// Carry on.
    #[default]
    Continue,
    /// Stop the operation with the given reason.
    Abort(String),
}

/// Observes and optionally vetoes what the framework does.
///
/// Every method has a no-op default so implementors only override what they
/// need. Hooks run in registration order; the first `Abort` wins.
#[async_trait::async_trait]
pub trait Hook: Send + Sync {
    /// Name for diagnostics.
    fn name(&self) -> &str;

    /// Called before a phase starts.
    async fn before_phase(&self, _phase: Phase, _task: &crate::Task) -> HookDecision {
        HookDecision::Continue
    }

    /// Called after a phase ends.
    async fn after_phase(&self, _phase: Phase, _task: &crate::Task, _success: bool) {}

    /// Called before a tool runs. May veto the call.
    async fn before_tool(
        &self,
        _ctx: &ToolContext,
        _tool: &str,
        _input: &serde_json::Value,
    ) -> HookDecision {
        HookDecision::Continue
    }

    /// Called after a tool ran.
    async fn after_tool(&self, _ctx: &ToolContext, _tool: &str, _output: &crate::ToolOutput) {}

    /// Extra text to append to an agent's system prompt (project rules,
    /// memory, …). Returning `None` adds nothing.
    async fn augment_prompt(&self, _role: &AgentRole, _task: &crate::Task) -> Option<String> {
        None
    }
}

/// Shared handle to a hook.
pub type SharedHook = Arc<dyn Hook>;

/// Everything a plugin can contribute, collected at start-up.
#[derive(Default, Clone)]
pub struct Registry {
    /// Tools by name.
    pub tools: ToolRegistry,
    /// Model providers by name.
    pub providers: BTreeMap<String, SharedProvider>,
    /// Agent specs by role name.
    pub agents: BTreeMap<String, AgentSpec>,
    /// Workspace providers by name.
    pub workspaces: BTreeMap<String, SharedWorkspaceProvider>,
    /// Memory stores by name.
    pub memories: BTreeMap<String, SharedMemory>,
    /// Hooks in registration order.
    pub hooks: Vec<SharedHook>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.tools)
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("workspaces", &self.workspaces.keys().collect::<Vec<_>>())
            .field("memories", &self.memories.keys().collect::<Vec<_>>())
            .field(
                "hooks",
                &self.hooks.iter().map(|h| h.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Registry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool.
    pub fn add_tool(&mut self, tool: SharedTool) -> &mut Self {
        self.tools.register(tool);
        self
    }

    /// Register a provider.
    pub fn add_provider(&mut self, name: impl Into<String>, provider: SharedProvider) -> &mut Self {
        self.providers.insert(name.into(), provider);
        self
    }

    /// Register (or override) an agent spec.
    pub fn add_agent(&mut self, spec: AgentSpec) -> &mut Self {
        self.agents.insert(spec.role.name(), spec);
        self
    }

    /// Register a workspace provider.
    pub fn add_workspace(&mut self, provider: SharedWorkspaceProvider) -> &mut Self {
        self.workspaces
            .insert(provider.name().to_string(), provider);
        self
    }

    /// Register a memory store.
    pub fn add_memory(&mut self, name: impl Into<String>, store: SharedMemory) -> &mut Self {
        self.memories.insert(name.into(), store);
        self
    }

    /// Register a hook.
    pub fn add_hook(&mut self, hook: SharedHook) -> &mut Self {
        self.hooks.push(hook);
        self
    }

    /// Agent spec for a role.
    #[must_use]
    pub fn agent(&self, role: &AgentRole) -> Option<&AgentSpec> {
        self.agents.get(&role.name())
    }

    /// Provider by name.
    #[must_use]
    pub fn provider(&self, name: &str) -> Option<&SharedProvider> {
        self.providers.get(name)
    }

    /// Run `before_phase` hooks; returns the first abort, if any.
    pub async fn before_phase(&self, phase: Phase, task: &crate::Task) -> HookDecision {
        for h in &self.hooks {
            if let HookDecision::Abort(r) = h.before_phase(phase, task).await {
                return HookDecision::Abort(format!("{}: {r}", h.name()));
            }
        }
        HookDecision::Continue
    }

    /// Run `after_phase` hooks.
    pub async fn after_phase(&self, phase: Phase, task: &crate::Task, success: bool) {
        for h in &self.hooks {
            h.after_phase(phase, task, success).await;
        }
    }

    /// Run `before_tool` hooks; returns the first abort, if any.
    pub async fn before_tool(
        &self,
        ctx: &ToolContext,
        tool: &str,
        input: &serde_json::Value,
    ) -> HookDecision {
        for h in &self.hooks {
            if let HookDecision::Abort(r) = h.before_tool(ctx, tool, input).await {
                return HookDecision::Abort(format!("{}: {r}", h.name()));
            }
        }
        HookDecision::Continue
    }

    /// Run `after_tool` hooks.
    pub async fn after_tool(&self, ctx: &ToolContext, tool: &str, output: &crate::ToolOutput) {
        for h in &self.hooks {
            h.after_tool(ctx, tool, output).await;
        }
    }

    /// Collect prompt augmentations from every hook.
    pub async fn augment_prompt(&self, role: &AgentRole, task: &crate::Task) -> Vec<String> {
        let mut out = Vec::new();
        for h in &self.hooks {
            if let Some(s) = h.augment_prompt(role, task).await {
                out.push(s);
            }
        }
        out
    }
}

/// A bundle of extensions.
///
/// In-process plugins implement this trait and are registered by the host
/// application; out-of-process plugins are adapted to it by the plugin host.
pub trait Plugin: Send + Sync {
    /// Unique plugin name.
    fn name(&self) -> &str;

    /// Semantic version.
    fn version(&self) -> &str {
        "0.0.0"
    }

    /// Contribute extensions to the registry.
    fn register(&self, registry: &mut Registry) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Veto;

    #[async_trait::async_trait]
    impl Hook for Veto {
        fn name(&self) -> &str {
            "veto"
        }
        async fn before_tool(
            &self,
            _c: &ToolContext,
            tool: &str,
            _i: &serde_json::Value,
        ) -> HookDecision {
            if tool == "bash" {
                HookDecision::Abort("no shell".into())
            } else {
                HookDecision::Continue
            }
        }
    }

    struct P;
    impl Plugin for P {
        fn name(&self) -> &str {
            "p"
        }
        fn register(&self, registry: &mut Registry) -> Result<()> {
            registry.add_hook(Arc::new(Veto));
            registry.add_agent(AgentSpec::new(AgentRole::Custom("x".into()), "hi"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn plugin_registers_and_hooks_veto() {
        let mut reg = Registry::new();
        P.register(&mut reg).unwrap();
        assert!(reg.agent(&AgentRole::Custom("x".into())).is_some());
        let ctx = ToolContext::new(".");
        assert!(matches!(
            reg.before_tool(&ctx, "bash", &serde_json::Value::Null)
                .await,
            HookDecision::Abort(_)
        ));
        assert_eq!(
            reg.before_tool(&ctx, "read_file", &serde_json::Value::Null)
                .await,
            HookDecision::Continue
        );
    }
}
