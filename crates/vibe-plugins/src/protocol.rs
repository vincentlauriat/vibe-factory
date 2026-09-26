//! The **Vibe Plugin Protocol** (VPP): JSON-RPC 2.0 over stdio.
//!
//! Framing is newline-delimited: every message is one JSON object on a single
//! line, terminated by `\n`. The host writes requests to the plugin's stdin
//! and reads responses and notifications from its stdout. Anything the plugin
//! writes to stderr is treated as free-form diagnostics.
//!
//! The message shapes are deliberately compatible with the Model Context
//! Protocol (MCP) so that an MCP stdio server can be used as a tool plugin:
//! the host sends both the VPP and the MCP initialisation fields, reads
//! camelCase aliases (`inputSchema`, `isError`, `serverInfo`) and accepts
//! capability objects (`{"tools": {}}`) as well as booleans.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use vibe_core::{AgentSpec, HookDecision, Permissions, ToolContext, ToolOutput};

/// JSON-RPC version string carried by every message.
pub const JSONRPC_VERSION: &str = "2.0";

/// Version of the Vibe Plugin Protocol implemented by this crate.
pub const PROTOCOL_VERSION: &str = "1";

/// MCP protocol revision advertised to MCP servers during initialisation.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Method and notification names.
pub mod methods {
    /// Handshake; first request sent by the host.
    pub const INITIALIZE: &str = "initialize";
    /// List the tools offered by the plugin.
    pub const TOOLS_LIST: &str = "tools/list";
    /// Invoke one tool.
    pub const TOOLS_CALL: &str = "tools/call";
    /// List the agent specs offered by the plugin.
    pub const AGENTS_LIST: &str = "agents/list";
    /// Ask the plugin whether a tool call may proceed.
    pub const HOOKS_BEFORE_TOOL: &str = "hooks/before_tool";
    /// Ask the plugin to exit.
    pub const SHUTDOWN: &str = "shutdown";
    /// Liveness probe (either side may send it; the answer is `{}`).
    pub const PING: &str = "ping";
    /// Log notification sent by a plugin (`{level, message}`).
    pub const LOG: &str = "log";
    /// MCP notification sent by the host after a successful `initialize`.
    pub const MCP_INITIALIZED: &str = "notifications/initialized";
    /// MCP log notification (`{level, data}`), accepted like [`LOG`].
    pub const MCP_LOG: &str = "notifications/message";
}

/// Identifier of a JSON-RPC request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric id (what this crate generates).
    Number(u64),
    /// String id (allowed by JSON-RPC, used by some peers).
    String(String),
}

impl From<u64> for RequestId {
    fn from(n: u64) -> Self {
        Self::Number(n)
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::String(s) => f.write_str(s),
        }
    }
}

/// Monotonic generator of request ids, starting at 1.
#[derive(Debug)]
pub struct IdCounter(AtomicU64);

impl Default for IdCounter {
    fn default() -> Self {
        Self(AtomicU64::new(1))
    }
}

impl IdCounter {
    /// New counter starting at 1.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Next id.
    pub fn next_id(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// A request expecting a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Correlation id.
    pub id: RequestId,
    /// Method name.
    pub method: String,
    /// Parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Build a request.
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// A one-way message (no id, no response).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Method name.
    pub method: String,
    /// Parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build a notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Error object of a failed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// Numeric error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON is not a valid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;

    /// Build an error.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `METHOD_NOT_FOUND` for `method`.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            Self::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    /// `INVALID_PARAMS` with a message.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// Answer to a [`Request`]: exactly one of `result` and `error` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Id of the request (null when the request could not be parsed).
    pub id: Option<RequestId>,
    /// Result on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Successful response.
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// Failed response.
    pub fn failure(id: Option<RequestId>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Any message that can appear on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A request.
    Request(Request),
    /// A notification.
    Notification(Notification),
    /// A response.
    Response(Response),
}

impl Message {
    /// Parse one line of the stream.
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        let value: Value = serde_json::from_str(line)?;
        Self::from_value(value)
    }

    /// Classify a JSON value: `method` + `id` is a request, `method` alone a
    /// notification, anything else a response.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        let has_method = value.get("method").is_some();
        let has_id = value.get("id").is_some_and(|id| !id.is_null());
        Ok(match (has_method, has_id) {
            (true, true) => Self::Request(serde_json::from_value(value)?),
            (true, false) => Self::Notification(serde_json::from_value(value)?),
            _ => Self::Response(serde_json::from_value(value)?),
        })
    }

    /// Serialise to a single line (without the trailing newline).
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        match self {
            Self::Request(r) => serde_json::to_string(r),
            Self::Notification(n) => serde_json::to_string(n),
            Self::Response(r) => serde_json::to_string(r),
        }
    }
}

/// Name and version of one side of the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Program name.
    pub name: String,
    /// Program version.
    #[serde(default)]
    pub version: String,
}

impl HostInfo {
    /// Build a host info.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    /// Identity of this framework as a plugin host.
    #[must_use]
    pub fn vibe_factory() -> Self {
        Self::new("vibe-factory", env!("CARGO_PKG_VERSION"))
    }
}

/// Parameters of `initialize`.
///
/// Besides the VPP fields (`protocol_version`, `host`) the host also sends
/// the MCP fields (`protocolVersion`, `clientInfo`, `capabilities`) so that
/// MCP stdio servers accept the handshake. VPP plugins ignore them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// VPP version, currently `"1"`.
    pub protocol_version: String,
    /// Identity of the host.
    pub host: HostInfo,
    /// MCP protocol revision (compatibility field).
    #[serde(
        rename = "protocolVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_protocol_version: Option<String>,
    /// MCP client identity (compatibility field).
    #[serde(
        rename = "clientInfo",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub client_info: Option<HostInfo>,
    /// MCP client capabilities (compatibility field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,
}

impl InitializeParams {
    /// Parameters with both the VPP and the MCP fields filled in.
    #[must_use]
    pub fn new(host: HostInfo) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            client_info: Some(host.clone()),
            host,
            mcp_protocol_version: Some(MCP_PROTOCOL_VERSION.to_string()),
            capabilities: Some(serde_json::json!({})),
        }
    }
}

/// What a plugin offers.
///
/// On the wire each flag is a boolean; when reading, an object (as sent by MCP
/// servers, e.g. `{"tools": {}}`) also counts as `true` and `null` as `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// The plugin answers `tools/list` and `tools/call`.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub tools: bool,
    /// The plugin answers `agents/list`.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub agents: bool,
    /// The plugin answers `hooks/before_tool`.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub hooks: bool,
}

impl Capabilities {
    /// Names of the enabled capabilities (`tools`, `agents`, `hooks`).
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        [
            ("tools", self.tools),
            ("agents", self.agents),
            ("hooks", self.hooks),
        ]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name.to_string())
        .collect()
    }

    /// Capabilities from a list of names; unknown names are ignored.
    pub fn from_names<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Self {
        let mut caps = Self::default();
        for name in names {
            match name.as_ref() {
                "tools" => caps.tools = true,
                "agents" => caps.agents = true,
                "hooks" => caps.hooks = true,
                _ => {}
            }
        }
        caps
    }
}

fn lenient_flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Null => false,
        _ => true,
    })
}

/// Result of `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitializeResult {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Offered capabilities.
    pub capabilities: Capabilities,
}

impl<'de> Deserialize<'de> for InitializeResult {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            version: Option<String>,
            #[serde(rename = "serverInfo", default)]
            server_info: Option<HostInfo>,
            #[serde(default)]
            capabilities: Capabilities,
        }
        let raw = Raw::deserialize(d)?;
        let info = raw.server_info;
        Ok(Self {
            name: raw
                .name
                .or_else(|| info.as_ref().map(|i| i.name.clone()))
                .unwrap_or_default(),
            version: raw
                .version
                .or_else(|| info.map(|i| i.version))
                .unwrap_or_default(),
            capabilities: raw.capabilities,
        })
    }
}

fn default_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

/// Optional behavioural hints about a tool (MCP `annotations`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolAnnotations {
    /// The tool does not modify its environment.
    #[serde(
        alias = "readOnlyHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
}

/// One entry of `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Tool name.
    pub name: String,
    /// Description shown to the model.
    #[serde(default)]
    pub description: String,
    /// JSON schema of the arguments (`inputSchema` is accepted too).
    #[serde(alias = "inputSchema", default = "default_schema")]
    pub input_schema: Value,
    /// Whether the tool mutates the workspace. When absent, the MCP
    /// `readOnlyHint` annotation is used, and failing that the tool is
    /// assumed to mutate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutating: Option<bool>,
    /// MCP annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

impl ToolDescriptor {
    /// Build a descriptor.
    pub fn new(name: impl Into<String>, description: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
            mutating: None,
            annotations: None,
        }
    }

    /// Effective mutating flag (conservative default: `true`).
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        self.mutating.unwrap_or_else(|| {
            !self
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false)
        })
    }
}

/// Result of `tools/list`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// Offered tools.
    #[serde(default)]
    pub tools: Vec<ToolDescriptor>,
    /// Pagination cursor (MCP); absent on the last page.
    #[serde(
        rename = "nextCursor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_cursor: Option<String>,
}

/// Key of the MCP-compatible metadata object in `tools/call` params.
pub const META_KEY: &str = "_meta";

/// Namespace of the framework's entry inside `_meta`.
pub const META_NAMESPACE: &str = "vibe";

/// Permission flags forwarded to a plugin tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolPermissions {
    /// May read files under the workspace root.
    #[serde(default)]
    pub read: bool,
    /// May create or modify files under the workspace root.
    #[serde(default)]
    pub write: bool,
    /// May execute commands.
    #[serde(default)]
    pub execute: bool,
    /// May reach the network.
    #[serde(default)]
    pub network: bool,
}

impl From<&Permissions> for ToolPermissions {
    fn from(p: &Permissions) -> Self {
        Self {
            read: p.read,
            write: p.write,
            execute: p.execute,
            network: p.network,
        }
    }
}

/// Invocation context sent with `tools/call` as `_meta.vibe`, so that a
/// plugin tool acts in the right workspace with the right permissions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolCallContext {
    /// Root directory the tool must stay inside (the task's workspace).
    pub workspace_root: PathBuf,
    /// Task being worked on, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Name of the calling agent.
    #[serde(default)]
    pub agent: String,
    /// Permissions granted to this invocation.
    #[serde(default)]
    pub permissions: ToolPermissions,
}

impl From<&ToolContext> for ToolCallContext {
    fn from(ctx: &ToolContext) -> Self {
        Self {
            workspace_root: ctx.workspace_root.clone(),
            task_id: ctx.task_id.map(|id| id.to_string()),
            agent: ctx.agent.clone(),
            permissions: ToolPermissions::from(&ctx.permissions),
        }
    }
}

/// Parameters of `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallParams {
    /// Tool name.
    pub name: String,
    /// Arguments object.
    #[serde(default)]
    pub arguments: Value,
    /// MCP-compatible metadata; the framework's context lives under
    /// [`META_NAMESPACE`].
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl ToolCallParams {
    /// Parameters without metadata.
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
            meta: None,
        }
    }

    /// Attach the invocation context as `_meta.vibe`.
    #[must_use]
    pub fn with_context(mut self, context: &ToolCallContext) -> Self {
        let mut meta = match self.meta.take() {
            Some(Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        if let Ok(v) = serde_json::to_value(context) {
            meta.insert(META_NAMESPACE.to_string(), v);
        }
        self.meta = Some(Value::Object(meta));
        self
    }

    /// The invocation context, when present and well-formed.
    #[must_use]
    pub fn context(&self) -> Option<ToolCallContext> {
        let v = self.meta.as_ref()?.get(META_NAMESPACE)?;
        serde_json::from_value(v.clone()).ok()
    }
}

/// One block of a tool result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentItem {
    /// Block type; only `"text"` is interpreted by the host.
    #[serde(rename = "type")]
    pub kind: String,
    /// Text of a `"text"` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ContentItem {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: Some(text.into()),
        }
    }
}

/// Result of `tools/call`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Content blocks.
    #[serde(default)]
    pub content: Vec<ContentItem>,
    /// Whether the call failed (`isError` is accepted too).
    #[serde(alias = "isError", default)]
    pub is_error: bool,
}

impl ToolCallResult {
    /// Concatenate the text blocks; other block types are replaced by a short
    /// placeholder.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .map(|c| match (&c.text, c.kind.as_str()) {
                (Some(t), "text") => t.clone(),
                (_, kind) => format!("[{kind} content omitted]"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Convert into a framework [`ToolOutput`].
    #[must_use]
    pub fn into_output(self) -> ToolOutput {
        let text = self.text();
        if self.is_error {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        }
    }
}

impl From<ToolOutput> for ToolCallResult {
    fn from(out: ToolOutput) -> Self {
        Self {
            content: vec![ContentItem::text(out.content)],
            is_error: out.is_error,
        }
    }
}

/// Result of `agents/list`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgentsListResult {
    /// Offered agent specs.
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
}

/// Parameters of `hooks/before_tool`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeToolParams {
    /// Tool about to run.
    pub tool: String,
    /// Its input.
    #[serde(default)]
    pub input: Value,
}

/// Verdict of a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Let the call proceed.
    #[default]
    Continue,
    /// Veto the call.
    Abort,
}

/// Result of `hooks/before_tool`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BeforeToolResult {
    /// Verdict.
    #[serde(default)]
    pub decision: Decision,
    /// Explanation (used when aborting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<BeforeToolResult> for HookDecision {
    fn from(r: BeforeToolResult) -> Self {
        match r.decision {
            Decision::Continue => HookDecision::Continue,
            Decision::Abort => {
                HookDecision::Abort(r.reason.unwrap_or_else(|| "vetoed by plugin".to_string()))
            }
        }
    }
}

impl From<HookDecision> for BeforeToolResult {
    fn from(d: HookDecision) -> Self {
        match d {
            HookDecision::Continue => Self::default(),
            HookDecision::Abort(reason) => Self {
                decision: Decision::Abort,
                reason: Some(reason),
            },
        }
    }
}

/// Parameters of the `log` notification (and of MCP `notifications/message`,
/// whose `data` field is used when `message` is absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogParams {
    /// `error`, `warn`/`warning`, `info`, `debug` or `trace`.
    #[serde(default)]
    pub level: String,
    /// Log text.
    #[serde(default)]
    pub message: String,
    /// MCP payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl LogParams {
    /// Text to log.
    #[must_use]
    pub fn text(&self) -> String {
        if !self.message.is_empty() {
            return self.message.clone();
        }
        match &self.data {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn message_classification() {
        let req = Message::parse(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert!(matches!(req, Message::Request(ref r) if r.id == RequestId::Number(1)));
        let note = Message::parse(r#"{"jsonrpc":"2.0","method":"log","params":{}}"#).unwrap();
        assert!(matches!(note, Message::Notification(_)));
        let resp = Message::parse(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#).unwrap();
        assert!(
            matches!(resp, Message::Response(ref r) if r.id == Some(RequestId::String("a".into())))
        );
        let err =
            Message::parse(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"x"}}"#)
                .unwrap();
        assert!(matches!(err, Message::Response(ref r) if r.error.is_some()));
        assert!(Message::parse("not json").is_err());
    }

    #[test]
    fn request_line_shape() {
        let line = Message::Request(Request::new(7u64, "shutdown", None))
            .to_line()
            .unwrap();
        assert_eq!(line, r#"{"jsonrpc":"2.0","id":7,"method":"shutdown"}"#);
        assert!(!line.contains('\n'));
    }

    #[test]
    fn initialize_params_carry_mcp_fields() {
        let v = serde_json::to_value(InitializeParams::new(HostInfo::new("h", "1.0"))).unwrap();
        assert_eq!(v["protocol_version"], "1");
        assert_eq!(v["host"]["name"], "h");
        assert_eq!(v["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(v["clientInfo"]["version"], "1.0");
    }

    #[test]
    fn initialize_result_reads_vpp_and_mcp_shapes() {
        let vpp: InitializeResult = serde_json::from_value(json!({
            "name": "p", "version": "1.2.3",
            "capabilities": {"tools": true, "hooks": false}
        }))
        .unwrap();
        assert_eq!(vpp.name, "p");
        assert!(vpp.capabilities.tools && !vpp.capabilities.hooks && !vpp.capabilities.agents);

        let mcp: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "serverInfo": {"name": "m", "version": "0.1"},
            "capabilities": {"tools": {"listChanged": true}, "logging": {}}
        }))
        .unwrap();
        assert_eq!(mcp.name, "m");
        assert_eq!(mcp.version, "0.1");
        assert!(mcp.capabilities.tools);
        assert!(!mcp.capabilities.agents);
    }

    #[test]
    fn tool_descriptor_aliases_and_mutating() {
        let d: ToolDescriptor = serde_json::from_value(json!({
            "name": "t", "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        }))
        .unwrap();
        assert_eq!(d.input_schema["type"], "object");
        assert!(!d.is_mutating());
        let bare: ToolDescriptor = serde_json::from_value(json!({"name": "u"})).unwrap();
        assert_eq!(bare.input_schema, json!({"type": "object"}));
        assert!(bare.is_mutating());
    }

    #[test]
    fn tool_call_result_conversion() {
        let r: ToolCallResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "a"}, {"type": "image", "data": "x"}],
            "isError": true
        }))
        .unwrap();
        let out = r.into_output();
        assert!(out.is_error);
        assert_eq!(out.content, "a\n[image content omitted]");
        let back = ToolCallResult::from(ToolOutput::ok("hi"));
        assert_eq!(back.text(), "hi");
        assert!(!back.is_error);
    }

    #[test]
    fn hook_decisions_roundtrip() {
        let r: BeforeToolResult =
            serde_json::from_value(json!({"decision": "abort", "reason": "no"})).unwrap();
        assert_eq!(HookDecision::from(r), HookDecision::Abort("no".into()));
        let r: BeforeToolResult = serde_json::from_value(json!({"decision": "continue"})).unwrap();
        assert_eq!(HookDecision::from(r), HookDecision::Continue);
        let v =
            serde_json::to_value(BeforeToolResult::from(HookDecision::Abort("x".into()))).unwrap();
        assert_eq!(v, json!({"decision": "abort", "reason": "x"}));
    }

    #[test]
    fn tool_call_context_travels_in_meta() {
        let mut perms = Permissions::read_only();
        perms.network = true;
        let mut ctx = ToolContext::new("/work/task-1").with_permissions(perms);
        ctx.agent = "coder".into();
        let params =
            ToolCallParams::new("t", json!({"x": 1})).with_context(&ToolCallContext::from(&ctx));
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["_meta"]["vibe"]["workspace_root"], "/work/task-1");
        assert_eq!(v["_meta"]["vibe"]["agent"], "coder");
        assert_eq!(
            v["_meta"]["vibe"]["permissions"],
            json!({"read": true, "write": false, "execute": false, "network": true})
        );
        assert!(v["_meta"]["vibe"].get("task_id").is_none());
        let back: ToolCallParams = serde_json::from_value(v).unwrap();
        let got = back.context().unwrap();
        assert_eq!(got.workspace_root, PathBuf::from("/work/task-1"));
        assert!(got.permissions.network && !got.permissions.write);
        assert!(ToolCallParams::new("t", json!({})).context().is_none());
        let foreign: ToolCallParams =
            serde_json::from_value(json!({"name": "t", "_meta": {"progressToken": 1}})).unwrap();
        assert!(foreign.context().is_none());
    }

    #[test]
    fn capability_names_roundtrip() {
        let caps = Capabilities::from_names(["hooks", "tools", "bogus"]);
        assert!(caps.tools && caps.hooks && !caps.agents);
        assert_eq!(caps.names(), vec!["tools", "hooks"]);
    }

    #[test]
    fn log_params_text() {
        let p: LogParams =
            serde_json::from_value(json!({"level": "info", "data": "hello"})).unwrap();
        assert_eq!(p.text(), "hello");
    }

    #[test]
    fn id_counter_is_monotonic() {
        let c = IdCounter::new();
        assert_eq!(c.next_id(), 1);
        assert_eq!(c.next_id(), 2);
    }
}
