//! Plugin side of the protocol: write a plugin in Rust.
//!
//! ```no_run
//! use vibe_core::{AgentRole, AgentSpec, HookDecision, ToolOutput};
//! use vibe_plugins::PluginServer;
//!
//! # async fn run() -> vibe_core::Result<()> {
//! PluginServer::new("greeter", "0.1.0")
//!     .tool(
//!         "greet",
//!         "Say hello to someone",
//!         serde_json::json!({
//!             "type": "object",
//!             "properties": { "who": { "type": "string" } },
//!             "required": ["who"]
//!         }),
//!         |args| async move {
//!             let who = args["who"].as_str().unwrap_or("world");
//!             Ok(ToolOutput::ok(format!("Hello, {who}!")))
//!         },
//!     )
//!     .agent(AgentSpec::new(AgentRole::Custom("greeter".into()), "You greet people."))
//!     .before_tool(|tool, _input| async move {
//!         if tool == "bash" {
//!             HookDecision::Abort("no shell here".into())
//!         } else {
//!             HookDecision::Continue
//!         }
//!     })
//!     .run_stdio()
//!     .await
//! # }
//! ```

use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use vibe_core::{AgentSpec, Error, HookDecision, Result, ToolOutput};

use crate::protocol::{
    AgentsListResult, BeforeToolParams, BeforeToolResult, Capabilities, ContentItem,
    InitializeResult, LogParams, Message, Notification, Response, RpcError, ToolCallContext,
    ToolCallParams, ToolCallResult, ToolDescriptor, ToolsListResult, methods,
};

/// Handler of one tool: receives the `arguments` object and the invocation
/// context sent by the host (`None` when the host sent none, e.g. an MCP
/// client).
pub type ToolHandler = Arc<
    dyn Fn(Value, Option<ToolCallContext>) -> BoxFuture<'static, Result<ToolOutput>> + Send + Sync,
>;

type OutgoingSlot = Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>;

/// Sends `log` notifications to the host from anywhere in a plugin (clone it
/// into tool handlers). Messages logged while the server is not serving are
/// dropped.
#[derive(Clone)]
pub struct PluginLogger {
    outgoing: OutgoingSlot,
}

impl std::fmt::Debug for PluginLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginLogger").finish_non_exhaustive()
    }
}

impl PluginLogger {
    /// Send a `log` notification (`level`: `error`, `warn`, `info`, `debug`
    /// or `trace`). Returns whether the message was queued.
    pub fn log(&self, level: &str, message: impl Into<String>) -> bool {
        let params = LogParams {
            level: level.to_string(),
            message: message.into(),
            data: None,
        };
        let Ok(params) = serde_json::to_value(params) else {
            return false;
        };
        let Ok(line) =
            Message::Notification(Notification::new(methods::LOG, Some(params))).to_line()
        else {
            return false;
        };
        let slot = self
            .outgoing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.as_ref().is_some_and(|tx| tx.send(line).is_ok())
    }
}

/// Handler of `hooks/before_tool`: receives the tool name and its input.
pub type BeforeToolHandler =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, HookDecision> + Send + Sync>;

/// Builder and runtime of a plugin speaking the Vibe Plugin Protocol.
///
/// Requests are handled concurrently; `shutdown` (or end of input) stops the
/// server after answering.
pub struct PluginServer {
    name: String,
    version: String,
    tools: IndexMap<String, (ToolDescriptor, ToolHandler)>,
    agents: Vec<AgentSpec>,
    before_tool: Option<BeforeToolHandler>,
    outgoing: OutgoingSlot,
}

impl std::fmt::Debug for PluginServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginServer")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("agents", &self.agents.len())
            .field("before_tool", &self.before_tool.is_some())
            .finish()
    }
}

impl PluginServer {
    /// Empty plugin.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tools: IndexMap::new(),
            agents: Vec::new(),
            before_tool: None,
            outgoing: Arc::new(Mutex::new(None)),
        }
    }

    /// Offer a tool. An `Err` from the handler is reported to the host as a
    /// failed tool result (`is_error: true`), not as a protocol error.
    #[must_use]
    pub fn tool<F, Fut>(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput>> + Send + 'static,
    {
        self.tool_with(
            ToolDescriptor::new(name, description, input_schema),
            handler,
        )
    }

    /// Offer a tool described by a full [`ToolDescriptor`] (e.g. to declare
    /// it non-mutating).
    #[must_use]
    pub fn tool_with<F, Fut>(mut self, descriptor: ToolDescriptor, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput>> + Send + 'static,
    {
        let handler: ToolHandler = Arc::new(move |args, _ctx| Box::pin(handler(args)));
        self.tools
            .insert(descriptor.name.clone(), (descriptor, handler));
        self
    }

    /// Offer a tool whose handler also receives the invocation context the
    /// host sent in `_meta.vibe` (workspace root, task, agent, permissions).
    #[must_use]
    pub fn tool_with_context<F, Fut>(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(Value, Option<ToolCallContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput>> + Send + 'static,
    {
        let descriptor = ToolDescriptor::new(name, description, input_schema);
        let handler: ToolHandler = Arc::new(move |args, ctx| Box::pin(handler(args, ctx)));
        self.tools
            .insert(descriptor.name.clone(), (descriptor, handler));
        self
    }

    /// Offer an agent spec.
    #[must_use]
    pub fn agent(mut self, spec: AgentSpec) -> Self {
        self.agents.push(spec);
        self
    }

    /// Install the `before_tool` hook.
    #[must_use]
    pub fn before_tool<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(String, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookDecision> + Send + 'static,
    {
        self.before_tool = Some(Arc::new(move |tool, input| Box::pin(handler(tool, input))));
        self
    }

    /// A handle sending `log` notifications to the host while serving.
    #[must_use]
    pub fn logger(&self) -> PluginLogger {
        PluginLogger {
            outgoing: Arc::clone(&self.outgoing),
        }
    }

    /// Send a `log` notification to the host (see [`PluginLogger::log`]).
    pub fn log(&self, level: &str, message: impl Into<String>) -> bool {
        self.logger().log(level, message)
    }

    /// Capabilities derived from what was registered.
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: !self.tools.is_empty(),
            agents: !self.agents.is_empty(),
            hooks: self.before_tool.is_some(),
        }
    }

    /// Serve on the process's stdin and stdout.
    ///
    /// Note: Tokio reads stdin on a blocking thread, so a runtime may only
    /// shut down once stdin is closed. The host closes it after `shutdown`;
    /// a binary can also call [`std::process::exit`] once this returns.
    pub async fn run_stdio(self) -> Result<()> {
        self.serve(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Serve on arbitrary streams until `shutdown` or end of input.
    pub async fn serve<R, W>(self, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let server = Arc::new(self);
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *server
            .outgoing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx.clone());
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                writer.write_all(line.as_bytes()).await?;
                writer.flush().await?;
            }
            Ok::<_, std::io::Error>(())
        });
        let send = |tx: &mpsc::UnboundedSender<String>, response: Response| {
            if let Ok(line) = Message::Response(response).to_line() {
                let _ = tx.send(line);
            }
        };

        let mut tasks = JoinSet::new();
        let mut lines = BufReader::new(reader).lines();
        let read_result = loop {
            while tasks.try_join_next().is_some() {}
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            };
            if line.trim().is_empty() {
                continue;
            }
            match Message::parse(&line) {
                Err(e) => send(
                    &tx,
                    Response::failure(None, RpcError::new(RpcError::PARSE_ERROR, e.to_string())),
                ),
                Ok(Message::Request(req)) if req.method == methods::SHUTDOWN => {
                    send(&tx, Response::success(req.id, serde_json::json!({})));
                    break Ok(());
                }
                Ok(Message::Request(req)) => {
                    let server = Arc::clone(&server);
                    let tx = tx.clone();
                    tasks.spawn(async move {
                        let response = match server.handle(&req.method, req.params).await {
                            Ok(result) => Response::success(req.id, result),
                            Err(err) => Response::failure(Some(req.id), err),
                        };
                        send(&tx, response);
                    });
                }
                // Notifications (e.g. `notifications/initialized`) and stray
                // responses need no answer.
                Ok(_) => {}
            }
        };
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        // Release every sender so the writer task drains and ends.
        server
            .outgoing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(tx);
        let write_result = writer_task
            .await
            .map_err(|e| Error::plugin(format!("plugin writer task failed: {e}")))?;
        read_result.map_err(|e| Error::plugin(format!("cannot read requests: {e}")))?;
        write_result.map_err(|e| Error::plugin(format!("cannot write responses: {e}")))?;
        Ok(())
    }

    /// Answer one request. Exposed for custom transports.
    pub async fn handle(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> std::result::Result<Value, RpcError> {
        let params = params.unwrap_or(Value::Null);
        match method {
            methods::INITIALIZE => to_value(InitializeResult {
                name: self.name.clone(),
                version: self.version.clone(),
                capabilities: self.capabilities(),
            }),
            methods::TOOLS_LIST => to_value(ToolsListResult {
                tools: self.tools.values().map(|(d, _)| d.clone()).collect(),
                next_cursor: None,
            }),
            methods::TOOLS_CALL => {
                let call: ToolCallParams = parse(params)?;
                let (_, handler) = self.tools.get(&call.name).ok_or_else(|| {
                    RpcError::invalid_params(format!("unknown tool `{}`", call.name))
                })?;
                let context = call.context();
                let result = match handler(call.arguments, context).await {
                    Ok(out) => ToolCallResult::from(out),
                    Err(e) => ToolCallResult {
                        content: vec![ContentItem::text(e.message)],
                        is_error: true,
                    },
                };
                to_value(result)
            }
            methods::AGENTS_LIST => to_value(AgentsListResult {
                agents: self.agents.clone(),
            }),
            methods::HOOKS_BEFORE_TOOL => {
                let p: BeforeToolParams = parse(params)?;
                let decision = match &self.before_tool {
                    Some(h) => h(p.tool, p.input).await,
                    None => HookDecision::Continue,
                };
                to_value(BeforeToolResult::from(decision))
            }
            methods::PING | methods::SHUTDOWN => Ok(serde_json::json!({})),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

fn to_value<T: serde::Serialize>(v: T) -> std::result::Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> std::result::Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::PluginProcess;
    use crate::protocol::HostInfo;
    use serde_json::json;
    use vibe_core::AgentRole;

    fn server() -> PluginServer {
        PluginServer::new("t", "9.9.9")
            .tool(
                "upper",
                "uppercase",
                json!({"type": "object"}),
                |a| async move {
                    match a["s"].as_str() {
                        Some(s) => Ok(ToolOutput::ok(s.to_uppercase())),
                        None => Err(Error::tool("missing `s`")),
                    }
                },
            )
            .agent(AgentSpec::new(AgentRole::Custom("a".into()), "p"))
    }

    #[tokio::test]
    async fn handle_dispatches_methods() {
        let s = server();
        let init = s.handle(methods::INITIALIZE, None).await.unwrap();
        assert_eq!(
            init["capabilities"],
            json!({"tools": true, "agents": true, "hooks": false})
        );
        let out = s
            .handle(
                methods::TOOLS_CALL,
                Some(json!({"name": "upper", "arguments": {}})),
            )
            .await
            .unwrap();
        assert_eq!(out["is_error"], true);
        let err = s
            .handle(methods::TOOLS_CALL, Some(json!({"name": "nope"})))
            .await
            .unwrap_err();
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        let hook = s
            .handle(
                methods::HOOKS_BEFORE_TOOL,
                Some(json!({"tool": "x", "input": {}})),
            )
            .await
            .unwrap();
        assert_eq!(hook["decision"], "continue");
        assert_eq!(
            s.handle("bogus", None).await.unwrap_err().code,
            RpcError::METHOD_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn client_and_server_over_duplex() {
        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (hr, hw) = tokio::io::split(host_side);
        let (pr, pw) = tokio::io::split(plugin_side);
        let serving = tokio::spawn(server().serve(pr, pw));
        let client = PluginProcess::from_streams("t", hr, hw);

        let info = client.initialize(HostInfo::new("test", "0")).await.unwrap();
        assert_eq!(info.version, "9.9.9");
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        let out = client
            .call_tool("upper", json!({"s": "abc"}))
            .await
            .unwrap();
        assert_eq!(out.into_output(), ToolOutput::ok("ABC"));
        let agents = client.list_agents().await.unwrap();
        assert_eq!(agents[0].role, AgentRole::Custom("a".into()));

        client.shutdown().await;
        serving.await.unwrap().unwrap();
        assert!(client.is_closed());
    }

    #[tokio::test]
    async fn context_and_logs_reach_the_right_side() {
        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (hr, mut hw) = tokio::io::split(host_side);
        let (pr, pw) = tokio::io::split(plugin_side);
        let server = PluginServer::new("ctx", "1").tool_with_context(
            "where",
            "Report the workspace root",
            json!({"type": "object"}),
            |_args, ctx| async move {
                Ok(ToolOutput::ok(match ctx {
                    Some(c) => c.workspace_root.display().to_string(),
                    None => "none".to_string(),
                }))
            },
        );
        let logger = server.logger();
        assert!(!logger.log("info", "before serving"), "dropped while idle");
        let serving = tokio::spawn(server.serve(pr, pw));

        let ctx = ToolCallContext {
            workspace_root: "/ws".into(),
            ..Default::default()
        };
        let call = ToolCallParams::new("where", json!({})).with_context(&ctx);
        let req = Message::Request(crate::protocol::Request::new(
            1u64,
            methods::TOOLS_CALL,
            Some(serde_json::to_value(call).unwrap()),
        ));
        hw.write_all(format!("{}\n", req.to_line().unwrap()).as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(hr).lines();
        let Message::Response(r) =
            Message::parse(&lines.next_line().await.unwrap().unwrap()).unwrap()
        else {
            panic!("expected a response");
        };
        assert_eq!(r.result.unwrap()["content"][0]["text"], "/ws");

        assert!(logger.log("warn", "careful"));
        let Message::Notification(n) =
            Message::parse(&lines.next_line().await.unwrap().unwrap()).unwrap()
        else {
            panic!("expected a notification");
        };
        assert_eq!(n.method, methods::LOG);
        assert_eq!(
            n.params.unwrap(),
            json!({"level": "warn", "message": "careful"})
        );

        hw.shutdown().await.unwrap();
        serving.await.unwrap().unwrap();
        assert!(!logger.log("info", "after serving"));
    }

    #[tokio::test]
    async fn parse_errors_are_reported() {
        let (host_side, plugin_side) = tokio::io::duplex(4096);
        let (hr, mut hw) = tokio::io::split(host_side);
        let (pr, pw) = tokio::io::split(plugin_side);
        let serving = tokio::spawn(server().serve(pr, pw));
        hw.write_all(b"{oops\n").await.unwrap();
        let mut lines = BufReader::new(hr).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let Message::Response(r) = Message::parse(&line).unwrap() else {
            panic!("expected a response");
        };
        assert_eq!(r.error.unwrap().code, RpcError::PARSE_ERROR);
        hw.shutdown().await.unwrap();
        serving.await.unwrap().unwrap();
    }
}
