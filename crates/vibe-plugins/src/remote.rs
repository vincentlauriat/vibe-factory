//! Adapters turning a plugin connection into framework extensions.

use std::sync::Arc;

use serde_json::Value;
use vibe_core::config::PluginConfig;
use vibe_core::{
    AgentSpec, Hook, HookDecision, Plugin, Registry, Result, Tool, ToolContext, ToolOutput,
};

use crate::client::PluginProcess;
use crate::protocol::{Capabilities, HostInfo, InitializeResult, ToolCallContext, ToolDescriptor};

/// A tool implemented by a plugin; calls are forwarded to `tools/call`.
#[derive(Debug, Clone)]
pub struct RemoteTool {
    process: Arc<PluginProcess>,
    descriptor: ToolDescriptor,
}

impl RemoteTool {
    /// Wrap a descriptor obtained from `tools/list`.
    #[must_use]
    pub fn new(process: Arc<PluginProcess>, descriptor: ToolDescriptor) -> Self {
        Self {
            process,
            descriptor,
        }
    }

    /// Name of the plugin providing the tool.
    #[must_use]
    pub fn plugin_name(&self) -> &str {
        self.process.name()
    }

    /// The descriptor advertised by the plugin.
    #[must_use]
    pub fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }
}

#[async_trait::async_trait]
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }

    fn input_schema(&self) -> Value {
        self.descriptor.input_schema.clone()
    }

    fn is_mutating(&self) -> bool {
        self.descriptor.is_mutating()
    }

    /// Forwards to `tools/call`, sending `ctx` as `_meta.vibe`.
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput> {
        Ok(self
            .process
            .call_tool_with_context(&self.descriptor.name, input, &ToolCallContext::from(ctx))
            .await?
            .into_output())
    }
}

/// A hook implemented by a plugin; `before_tool` is forwarded to
/// `hooks/before_tool`.
///
/// The hook **fails closed**: if the plugin cannot be reached or answers
/// with an error, the tool call is aborted.
#[derive(Debug, Clone)]
pub struct RemoteHook {
    process: Arc<PluginProcess>,
}

impl RemoteHook {
    /// Wrap a plugin that advertised the `hooks` capability.
    #[must_use]
    pub fn new(process: Arc<PluginProcess>) -> Self {
        Self { process }
    }
}

#[async_trait::async_trait]
impl Hook for RemoteHook {
    fn name(&self) -> &str {
        self.process.name()
    }

    async fn before_tool(&self, _ctx: &ToolContext, tool: &str, input: &Value) -> HookDecision {
        match self.process.before_tool(tool, input.clone()).await {
            Ok(result) => result.into(),
            Err(e) => {
                tracing::warn!(plugin = %self.process.name(), "before_tool hook failed: {e}");
                HookDecision::Abort(format!("hook plugin unavailable: {}", e.message))
            }
        }
    }
}

/// An initialised out-of-process plugin, with the tools and agent specs it
/// advertised at start-up.
#[derive(Debug)]
pub struct RemotePlugin {
    process: Arc<PluginProcess>,
    info: InitializeResult,
    tools: Vec<ToolDescriptor>,
    agents: Vec<AgentSpec>,
}

impl RemotePlugin {
    /// Spawn the plugin described by `config` and [`connect`](Self::connect)
    /// to it. The process is killed if the handshake fails.
    pub async fn start(config: &PluginConfig, host: HostInfo) -> Result<Self> {
        let process = PluginProcess::spawn(config)?;
        Self::connect(process, host).await
    }

    /// Perform the handshake, then fetch the tools and agent specs the
    /// plugin advertised.
    pub async fn connect(process: PluginProcess, host: HostInfo) -> Result<Self> {
        let process = Arc::new(process);
        match Self::discover(&process, host).await {
            Ok((info, tools, agents)) => Ok(Self {
                process,
                info,
                tools,
                agents,
            }),
            Err(e) => {
                process.shutdown().await;
                Err(e)
            }
        }
    }

    async fn discover(
        process: &PluginProcess,
        host: HostInfo,
    ) -> Result<(InitializeResult, Vec<ToolDescriptor>, Vec<AgentSpec>)> {
        let info = process.initialize(host).await?;
        let tools = if info.capabilities.tools {
            process.list_tools().await?
        } else {
            Vec::new()
        };
        let agents = if info.capabilities.agents {
            process.list_agents().await?
        } else {
            Vec::new()
        };
        Ok((info, tools, agents))
    }

    /// Name the plugin announced about itself (may differ from the
    /// configured name returned by [`Plugin::name`]).
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.info.name
    }

    /// Handshake result.
    #[must_use]
    pub fn info(&self) -> &InitializeResult {
        &self.info
    }

    /// Advertised capabilities.
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.info.capabilities
    }

    /// Advertised tools.
    #[must_use]
    pub fn tools(&self) -> &[ToolDescriptor] {
        &self.tools
    }

    /// Advertised agent specs.
    #[must_use]
    pub fn agents(&self) -> &[AgentSpec] {
        &self.agents
    }

    /// The underlying connection.
    #[must_use]
    pub fn process(&self) -> &Arc<PluginProcess> {
        &self.process
    }

    /// Stop the plugin (see [`PluginProcess::shutdown`]).
    pub async fn shutdown(&self) {
        self.process.shutdown().await;
    }
}

impl Plugin for RemotePlugin {
    fn name(&self) -> &str {
        self.process.name()
    }

    fn version(&self) -> &str {
        &self.info.version
    }

    fn register(&self, registry: &mut Registry) -> Result<()> {
        for descriptor in &self.tools {
            if registry.tools.get(&descriptor.name).is_some() {
                tracing::warn!(
                    plugin = %self.name(),
                    "tool `{}` overrides an already registered tool",
                    descriptor.name
                );
            }
            registry.add_tool(Arc::new(RemoteTool::new(
                Arc::clone(&self.process),
                descriptor.clone(),
            )));
        }
        for spec in &self.agents {
            registry.add_agent(spec.clone());
        }
        if self.info.capabilities.hooks {
            registry.add_hook(Arc::new(RemoteHook::new(Arc::clone(&self.process))));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Message, Response};
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A minimal MCP stdio server written by hand, answering with MCP field
    /// names (`serverInfo`, `inputSchema`, `isError`, capability objects) and
    /// rejecting every non-MCP method.
    fn fake_mcp_server() -> PluginProcess {
        let (host_side, server_side) = tokio::io::duplex(64 * 1024);
        let (hr, hw) = tokio::io::split(host_side);
        let (sr, mut sw) = tokio::io::split(server_side);
        tokio::spawn(async move {
            let mut lines = BufReader::new(sr).lines();
            let mut initialized = false;
            while let Ok(Some(line)) = lines.next_line().await {
                let msg = Message::parse(&line).unwrap();
                let req = match msg {
                    Message::Notification(n) => {
                        initialized |= n.method == "notifications/initialized";
                        continue;
                    }
                    Message::Request(r) => r,
                    Message::Response(_) => continue,
                };
                let params = req.params.unwrap_or(Value::Null);
                let result = match req.method.as_str() {
                    "initialize" => {
                        assert_eq!(params["protocolVersion"], crate::MCP_PROTOCOL_VERSION);
                        Some(json!({
                            "protocolVersion": "2025-06-18",
                            "serverInfo": {"name": "mcp-fake", "version": "3.1"},
                            "capabilities": {"tools": {"listChanged": false}}
                        }))
                    }
                    "tools/list" => {
                        assert!(initialized, "tools/list before notifications/initialized");
                        Some(json!({"tools": [{
                            "name": "add",
                            "description": "Add two numbers",
                            "inputSchema": {"type": "object", "properties": {
                                "a": {"type": "number"}, "b": {"type": "number"}}},
                            "annotations": {"readOnlyHint": true}
                        }]}))
                    }
                    "tools/call" => {
                        let a = params["arguments"]["a"].as_f64();
                        let b = params["arguments"]["b"].as_f64();
                        Some(match (a, b) {
                            (Some(a), Some(b)) => json!({
                                "content": [{"type": "text", "text": format!("{}", a + b)}],
                                "isError": false
                            }),
                            _ => json!({
                                "content": [{"type": "text", "text": "a and b are required"}],
                                "isError": true
                            }),
                        })
                    }
                    _ => None,
                };
                let response = match result {
                    Some(r) => Response::success(req.id, r),
                    None => Response::failure(
                        Some(req.id),
                        crate::RpcError::method_not_found(&req.method),
                    ),
                };
                let mut out = Message::Response(response).to_line().unwrap();
                out.push('\n');
                sw.write_all(out.as_bytes()).await.unwrap();
                sw.flush().await.unwrap();
            }
        });
        PluginProcess::from_streams("mcp", hr, hw)
    }

    #[tokio::test]
    async fn mcp_server_works_as_tool_plugin() {
        let plugin = RemotePlugin::connect(fake_mcp_server(), HostInfo::vibe_factory())
            .await
            .unwrap();
        assert_eq!(plugin.name(), "mcp");
        assert_eq!(plugin.server_name(), "mcp-fake");
        assert_eq!(plugin.version(), "3.1");
        assert!(plugin.capabilities().tools);
        assert!(!plugin.capabilities().hooks);

        let mut reg = Registry::new();
        plugin.register(&mut reg).unwrap();
        assert!(reg.hooks.is_empty());
        assert!(reg.agents.is_empty());
        let tool = reg.tools.get("add").unwrap();
        assert!(!tool.is_mutating());
        assert_eq!(tool.input_schema()["properties"]["a"]["type"], "number");

        let ctx = ToolContext::new(".");
        let ok = tool.call(&ctx, json!({"a": 2, "b": 3})).await.unwrap();
        assert_eq!(ok, ToolOutput::ok("5"));
        let bad = tool.call(&ctx, json!({})).await.unwrap();
        assert!(bad.is_error);

        // `shutdown` is not an MCP method: the error is tolerated.
        plugin.shutdown().await;
    }
}
