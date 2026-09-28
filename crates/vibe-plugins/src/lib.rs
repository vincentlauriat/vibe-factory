//! # vibe-plugins
//!
//! Plugin discovery, manifests and the out-of-process plugin protocol of
//! **Vibe Factory**.
//!
//! A plugin contributes **tools**, **agent specs** and a **`before_tool`
//! hook** to the framework's [`Registry`](vibe_core::Registry). It can live
//! in-process (any [`vibe_core::Plugin`], added with
//! [`PluginHost::add_native`]) or in a separate process written in any
//! language, speaking the **Vibe Plugin Protocol** (VPP) described below.
//!
//! ## Loading plugins
//!
//! ```no_run
//! use vibe_core::{Registry, VibeConfig};
//! use vibe_plugins::{PluginHost, discovery};
//!
//! # async fn run(project: &std::path::Path) -> vibe_core::Result<()> {
//! let config = VibeConfig::load(project)?;
//! let configs = discovery::plugin_configs(project, &config);
//! let mut host = PluginHost::load(&configs).await?;
//! let mut registry = Registry::new();
//! host.register_all(&mut registry)?;
//! // ... run the pipeline with `registry` ...
//! host.shutdown_all().await;
//! # Ok(())
//! # }
//! ```
//!
//! Each plugin process runs in the `cwd` of its configuration: the
//! manifest's directory for discovered plugins, the host's current directory
//! otherwise. A program written as a relative path (`./bin/plugin`) is
//! resolved against it.
//!
//! Plugins are found in `<project>/.vibe/plugins/<dir>/vibe-plugin.toml` and
//! in `<user config dir>/vibe/plugins/<dir>/vibe-plugin.toml`, and can also
//! be declared inline in `.vibe/config.toml` under `[[plugins]]`. Inline
//! declarations shadow project manifests, which shadow user manifests. See
//! [`manifest`] for the manifest format.
//!
//! ## The protocol
//!
//! VPP is JSON-RPC 2.0 over the plugin's stdin and stdout, one JSON object
//! per line (newline-delimited, UTF-8). The host sends requests; the plugin
//! answers each with a response carrying the same `id`, in any order.
//! Whatever the plugin writes to stderr is logged by the host at debug
//! level.
//!
//! | Method | Params | Result |
//! |--------|--------|--------|
//! | `initialize` | `{protocol_version: "1", host: {name, version}}` | `{name, version, capabilities: {tools?, agents?, hooks?}}` |
//! | `tools/list` | `{}` | `{tools: [{name, description, input_schema}]}` |
//! | `tools/call` | `{name, arguments, _meta?: {vibe: ToolCallContext}}` | `{content: [{type: "text", text}], is_error?}` |
//! | `agents/list` | `{}` | `{agents: [AgentSpec]}` |
//! | `hooks/before_tool` | `{tool, input}` | `{decision: "continue" \| "abort", reason?}` |
//! | `shutdown` | none | `{}`, then the plugin exits |
//!
//! The plugin may send one notification, `log` with `{level, message}`
//! (`level` is `error`, `warn`, `info`, `debug` or `trace`); from Rust, use
//! [`PluginServer::logger`]. The host only
//! calls `tools/*`, `agents/list` and `hooks/before_tool` when the matching
//! capability was advertised by `initialize`. After `initialize` the host
//! also sends the notification `notifications/initialized`, which plugins
//! may ignore. Unknown methods must be answered with the JSON-RPC error
//! `-32601`.
//!
//! An exchange with the bundled `vibe-echo-plugin` (`>` host to plugin, `<`
//! plugin to host; the `log` line only illustrates where a notification may
//! appear, the echo plugin does not send one):
//!
//! ```json
//! > {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":"1","host":{"name":"vibe-factory","version":"0.5.0"}}}
//! < {"jsonrpc":"2.0","id":1,"result":{"name":"echo","version":"0.1.0","capabilities":{"tools":true,"agents":true,"hooks":true}}}
//! > {"jsonrpc":"2.0","method":"notifications/initialized"}
//! > {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
//! < {"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Return the given text unchanged.","input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}
//! > {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}
//! < {"jsonrpc":"2.0","method":"log","params":{"level":"info","message":"echoing 2 bytes"}}
//! < {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hi"}],"is_error":false}}
//! > {"jsonrpc":"2.0","id":4,"method":"hooks/before_tool","params":{"tool":"forbidden","input":{}}}
//! < {"jsonrpc":"2.0","id":4,"result":{"decision":"abort","reason":"the echo plugin forbids this tool"}}
//! > {"jsonrpc":"2.0","id":5,"method":"shutdown"}
//! < {"jsonrpc":"2.0","id":5,"result":{}}
//! ```
//!
//! ### Tool call context
//!
//! With every `tools/call` the host sends the invocation context in the
//! MCP-compatible `_meta` object, under the `vibe` key, so that a plugin tool
//! works in the task's workspace with the task's permissions:
//!
//! ```json
//! {"name": "echo", "arguments": {"text": "hi"},
//!  "_meta": {"vibe": {"workspace_root": "/repo/.vibe/worktrees/task-1",
//!                     "task_id": "6f1c3e0a-…", "agent": "coder",
//!                     "permissions": {"read": true, "write": true,
//!                                     "execute": true, "network": false}}}}
//! ```
//!
//! `task_id` is omitted when there is no task. Plugins that do not care may
//! ignore `_meta`; MCP servers do. In Rust, register the tool with
//! [`PluginServer::tool_with_context`] to receive it as a
//! [`ToolCallContext`].
//!
//! Timeouts: `tools/call` gets 600 s, every other request 60 s. On
//! shutdown the host waits 2 s for the answer, closes the plugin's stdin,
//! waits 2 s more for the process to exit, then kills it.
//!
//! ### Agent specs on the wire
//!
//! An agent spec is the JSON form of [`vibe_core::AgentSpec`]. Only `role`
//! and `system_prompt` are required. A role is a plain `snake_case` string:
//! a built-in one such as `"coder"` or `"qa_reviewer"`, or any new name,
//! which becomes a custom role:
//!
//! ```json
//! {"role": "echo_agent",
//!  "system_prompt": "You repeat what you are told.",
//!  "description": "Echoes its instructions",
//!  "tools": {"named": ["echo"]},
//!  "thinking": "low",
//!  "max_steps": 20}
//! ```
//!
//! ### Hooks
//!
//! A `before_tool` hook sees every tool call of every agent and may veto it.
//! Remote hooks fail closed: if the plugin cannot answer, the call is
//! aborted.
//!
//! ## Writing a plugin in Rust
//!
//! Use [`PluginServer`]; the crate ships `vibe-echo-plugin` as a complete
//! example.
//!
//! ```no_run
//! use vibe_core::ToolOutput;
//! use vibe_plugins::PluginServer;
//!
//! #[tokio::main]
//! async fn main() -> vibe_core::Result<()> {
//!     PluginServer::new("shout", "0.1.0")
//!         .tool(
//!             "shout",
//!             "Uppercase a text",
//!             serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
//!             |args| async move {
//!                 let text = args["text"].as_str().unwrap_or_default();
//!                 Ok(ToolOutput::ok(text.to_uppercase()))
//!             },
//!         )
//!         .run_stdio()
//!         .await
//! }
//! ```
//!
//! Then declare it in `.vibe/plugins/shout/vibe-plugin.toml`:
//!
//! ```toml
//! name = "shout"
//! version = "0.1.0"
//! command = ["./target/release/shout"]
//! ```
//!
//! ## Writing a plugin in any language
//!
//! Read lines from stdin, parse each as JSON, and for every object that has
//! both `method` and `id` write exactly one line to stdout with
//! `{"jsonrpc": "2.0", "id": <same id>, "result": ...}` (or `"error":
//! {"code", "message"}`), then flush. Ignore objects without an `id`. Answer
//! the methods of the table above for the capabilities you advertise, and
//! exit on `shutdown` or at end of input. Write diagnostics to stderr, never
//! to stdout.
//!
//! ## Using an MCP stdio server as a tool plugin
//!
//! The message shapes are compatible with the Model Context Protocol: the
//! host also sends the MCP handshake fields (`protocolVersion`,
//! `clientInfo`, `capabilities`) and `notifications/initialized`, and reads
//! `serverInfo`, `inputSchema`, `isError`, `nextCursor`, capability objects
//! and MCP `notifications/message` logs. Point a plugin declaration at the
//! server's command:
//!
//! ```toml
//! [[plugins]]
//! name = "my-mcp-tools"
//! command = ["my-mcp-server", "--stdio"]
//! ```
//!
//! Its tools are registered like any other; MCP servers expose neither
//! agents nor hooks, and the unanswered `shutdown` request is tolerated.

pub mod client;
pub mod discovery;
pub mod host;
pub mod manifest;
pub mod protocol;
pub mod remote;
pub mod server;

pub use client::{
    DEFAULT_TIMEOUT, PluginProcess, SHUTDOWN_GRACE, TOOL_CALL_TIMEOUT, resolve_program,
};
pub use discovery::{discover, merge_with_config, plugin_configs};
pub use host::{LoadedPlugin, PluginHost};
pub use manifest::{MANIFEST_FILE, PluginManifest};
pub use protocol::{
    Capabilities, HostInfo, InitializeResult, MCP_PROTOCOL_VERSION, PROTOCOL_VERSION, RpcError,
    ToolCallContext, ToolCallResult, ToolDescriptor, ToolPermissions,
};
pub use remote::{RemoteHook, RemotePlugin, RemoteTool};
pub use server::{PluginLogger, PluginServer};
