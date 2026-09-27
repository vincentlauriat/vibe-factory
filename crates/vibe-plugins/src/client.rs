//! Host side of the protocol: a connection to one plugin.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use vibe_core::config::PluginConfig;
use vibe_core::{AgentSpec, Error, Result};

use crate::protocol::{
    AgentsListResult, BeforeToolParams, BeforeToolResult, HostInfo, IdCounter, InitializeParams,
    InitializeResult, LogParams, Message, Notification, Request, Response, RpcError,
    ToolCallContext, ToolCallParams, ToolCallResult, ToolDescriptor, ToolsListResult, methods,
};

/// Timeout applied to every request except `tools/call`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeout applied to `tools/call`.
pub const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// How long [`PluginProcess::shutdown`] waits for the answer to `shutdown`
/// and then for the process to exit before killing it.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Upper bound on `tools/list` pages, to survive a plugin that loops.
const MAX_TOOL_PAGES: usize = 100;

/// Framed lines queued for the writer task before senders wait.
const OUTBOX_CAPACITY: usize = 64;

type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;
type PendingMap = HashMap<u64, oneshot::Sender<Result<Value>>>;

/// Work for the writer task.
enum Outgoing {
    /// A complete framed line, and where to report whether it was written.
    Line {
        bytes: Vec<u8>,
        ack: oneshot::Sender<Result<()>>,
    },
    /// Close the plugin's stdin, then report back and stop.
    Close(oneshot::Sender<()>),
}

/// Lock a std mutex, ignoring poisoning: a panic elsewhere while holding it
/// leaves the guarded data consistent for our uses.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct PendingState {
    pending: PendingMap,
    closed: bool,
}

struct Shared {
    name: String,
    /// Queue of the writer task, the only owner of the plugin's stdin.
    /// `None` once [`PluginProcess::shutdown`] closed it.
    outbox: Mutex<Option<mpsc::Sender<Outgoing>>>,
    state: Mutex<PendingState>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, PendingState> {
        lock(&self.state)
    }

    fn closed_error(&self) -> Error {
        Error::plugin(format!("plugin `{}` is closed", self.name))
    }

    /// Queue `message` as one complete line and wait until the writer task
    /// has written it. Dropping this future never splits a line: the line
    /// is written whole or, if its turn has not come yet, not at all.
    async fn write_message(&self, message: &Message) -> Result<()> {
        let mut line = message.to_line()?;
        line.push('\n');
        let outbox = lock(&self.outbox).clone();
        let outbox = outbox.ok_or_else(|| self.closed_error())?;
        let (ack, written) = oneshot::channel();
        let bytes = line.into_bytes();
        outbox
            .send(Outgoing::Line { bytes, ack })
            .await
            .map_err(|_| self.closed_error())?;
        drop(outbox);
        // No answer means the writer task has stopped.
        written.await.map_err(|_| self.closed_error())?
    }

    /// Mark the connection dead and fail every pending call with `why`.
    fn close(&self, why: &str) {
        let drained: Vec<_> = {
            let mut st = self.state();
            st.closed = true;
            st.pending.drain().map(|(_, tx)| tx).collect()
        };
        for tx in drained {
            let _ = tx.send(Err(Error::plugin(format!("plugin `{}` {why}", self.name))));
        }
    }
}

/// Owns the plugin's stdin and writes queued lines one at a time, so a
/// cancelled caller can never leave a partial line behind. It holds only a
/// weak reference to `shared`, and ends once every sender is gone.
async fn write_loop(shared: Weak<Shared>, mut writer: BoxWriter, mut rx: mpsc::Receiver<Outgoing>) {
    while let Some(item) = rx.recv().await {
        match item {
            Outgoing::Line { bytes, ack } => {
                if ack.is_closed() {
                    // The caller gave up before its turn: send nothing.
                    continue;
                }
                let io = async {
                    writer.write_all(&bytes).await?;
                    writer.flush().await
                };
                if let Err(e) = io.await {
                    let name = shared.upgrade().map_or_else(String::new, |s| {
                        s.close("stopped accepting input");
                        s.name.clone()
                    });
                    let err = Error::plugin(format!("cannot write to plugin `{name}`: {e}"));
                    let _ = ack.send(Err(err.with_source(e)));
                    // Dropping `rx` fails every queued and later write.
                    return;
                }
                let _ = ack.send(Ok(()));
            }
            Outgoing::Close(done) => {
                let _ = writer.shutdown().await;
                let _ = done.send(());
                return;
            }
        }
    }
    let _ = writer.shutdown().await;
}

/// Removes a request from the pending map when dropped.
struct PendingGuard<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.shared.state().pending.remove(&self.id);
    }
}

/// A running plugin (a child process, or any pair of byte streams) speaking
/// the Vibe Plugin Protocol.
///
/// Requests are multiplexed: several calls may be in flight at once and
/// responses are routed back by id by a background task.
pub struct PluginProcess {
    shared: Arc<Shared>,
    ids: IdCounter,
    child: tokio::sync::Mutex<Option<Child>>,
    writer_task: Mutex<Option<JoinHandle<()>>>,
    info: OnceLock<InitializeResult>,
}

impl std::fmt::Debug for PluginProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProcess")
            .field("name", &self.shared.name)
            .field("info", &self.info.get())
            .finish_non_exhaustive()
    }
}

impl PluginProcess {
    /// Start the plugin described by `config` (`command[0]` is the program,
    /// the rest its arguments) with the extra environment variables of the
    /// configuration, in `config.cwd` (default: the current directory). A
    /// program given as a relative path is resolved with [`resolve_program`].
    /// The process is killed when the handle is dropped.
    ///
    /// Must be called from within a Tokio runtime.
    pub fn spawn(config: &PluginConfig) -> Result<Self> {
        let (program, args) = config.command.split_first().ok_or_else(|| {
            Error::plugin(format!("plugin `{}` has an empty command", config.name))
        })?;
        let cwd = match &config.cwd {
            Some(dir) => dir.clone(),
            None => std::env::current_dir().map_err(|e| {
                Error::plugin(format!(
                    "plugin `{}`: cannot determine the working directory: {e}",
                    config.name
                ))
                .with_source(e)
            })?,
        };
        let program = resolve_program(program, &cwd);
        let mut cmd = Command::new(&program);
        cmd.args(args)
            .current_dir(&cwd)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            Error::plugin(format!(
                "cannot start plugin `{}` (`{}` in `{}`): {e}",
                config.name,
                program.display(),
                cwd.display()
            ))
            .with_source(e)
        })?;
        let missing = || Error::plugin(format!("plugin `{}`: missing stdio pipe", config.name));
        let stdin = child.stdin.take().ok_or_else(missing)?;
        let stdout = child.stdout.take().ok_or_else(missing)?;
        if let Some(stderr) = child.stderr.take() {
            let name = config.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("[{name}] {line}");
                }
            });
        }
        let process = Self::from_streams(config.name.clone(), stdout, stdin);
        *process.child.try_lock().map_err(|_| missing())? = Some(child);
        Ok(process)
    }

    /// Connect over arbitrary streams: `reader` carries what the plugin
    /// writes, `writer` what the host sends. Useful for in-process plugins
    /// and tests (see [`tokio::io::duplex`]).
    ///
    /// Must be called from within a Tokio runtime.
    pub fn from_streams<R, W>(name: impl Into<String>, reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (outbox, rx) = mpsc::channel(OUTBOX_CAPACITY);
        let shared = Arc::new(Shared {
            name: name.into(),
            outbox: Mutex::new(Some(outbox)),
            state: Mutex::new(PendingState::default()),
        });
        let writer_task = tokio::spawn(write_loop(Arc::downgrade(&shared), Box::new(writer), rx));
        tokio::spawn(read_loop(Arc::clone(&shared), reader));
        Self {
            shared,
            ids: IdCounter::new(),
            child: tokio::sync::Mutex::new(None),
            writer_task: Mutex::new(Some(writer_task)),
            info: OnceLock::new(),
        }
    }

    /// Name the host knows the plugin by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Result of the handshake, once [`initialize`](Self::initialize) ran.
    #[must_use]
    pub fn info(&self) -> Option<&InitializeResult> {
        self.info.get()
    }

    /// Whether the plugin's output stream has ended.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.state().closed
    }

    /// Send a request and wait for its result, using [`TOOL_CALL_TIMEOUT`]
    /// for `tools/call` and [`DEFAULT_TIMEOUT`] otherwise. A `null` `params`
    /// is omitted from the request.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let timeout = if method == methods::TOOLS_CALL {
            TOOL_CALL_TIMEOUT
        } else {
            DEFAULT_TIMEOUT
        };
        self.call_with_timeout(method, params, timeout).await
    }

    /// Send a request and wait at most `timeout` for its result.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.ids.next_id();
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.shared.state();
            if st.closed {
                return Err(Error::plugin(format!("plugin `{}` is closed", self.name())));
            }
            st.pending.insert(id, tx);
        }
        // Removes the entry however this call ends, including when the
        // caller's future is dropped while waiting.
        let _pending = PendingGuard {
            shared: &self.shared,
            id,
        };
        let params = (!params.is_null()).then_some(params);
        let request = Message::Request(Request::new(id, method, params));
        // The write is covered by the timeout too: a plugin that stops
        // reading its stdin must not block the caller forever once the pipe
        // buffer is full. Giving up mid-write is safe: the writer task
        // finishes the line on its own.
        let exchange = async {
            self.shared.write_message(&request).await?;
            rx.await.map_err(|_| {
                Error::plugin(format!(
                    "plugin `{}` dropped request `{method}`",
                    self.name()
                ))
            })?
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(Error::plugin(format!(
                "plugin `{}` did not answer `{method}` within {}s",
                self.name(),
                timeout.as_secs_f32()
            ))),
        }
    }

    /// Typed variant of [`call`](Self::call).
    pub async fn request<P, R>(&self, method: &str, params: &P) -> Result<R>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let value = self.call(method, serde_json::to_value(params)?).await?;
        serde_json::from_value(value).map_err(|e| {
            Error::plugin(format!(
                "plugin `{}` sent an invalid `{method}` result: {e}",
                self.name()
            ))
            .with_source(e)
        })
    }

    /// Send a notification (no response expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let params = (!params.is_null()).then_some(params);
        self.shared
            .write_message(&Message::Notification(Notification::new(method, params)))
            .await
    }

    /// Perform the handshake and remember its result. Also sends the MCP
    /// `notifications/initialized` notification, which VPP plugins ignore.
    pub async fn initialize(&self, host: HostInfo) -> Result<InitializeResult> {
        let result: InitializeResult = self
            .request(methods::INITIALIZE, &InitializeParams::new(host))
            .await?;
        self.notify(methods::MCP_INITIALIZED, Value::Null).await?;
        let _ = self.info.set(result.clone());
        Ok(result)
    }

    /// `tools/list`, following MCP pagination cursors.
    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            let page: ToolsListResult = self.request(methods::TOOLS_LIST, &params).await?;
            tools.extend(page.tools);
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(tools)
    }

    /// `agents/list`.
    pub async fn list_agents(&self) -> Result<Vec<AgentSpec>> {
        let result: AgentsListResult = self
            .request(methods::AGENTS_LIST, &serde_json::json!({}))
            .await?;
        Ok(result.agents)
    }

    /// `tools/call` without invocation context.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolCallResult> {
        self.request(methods::TOOLS_CALL, &ToolCallParams::new(name, arguments))
            .await
    }

    /// `tools/call` with the invocation context sent as `_meta.vibe`.
    pub async fn call_tool_with_context(
        &self,
        name: &str,
        arguments: Value,
        context: &ToolCallContext,
    ) -> Result<ToolCallResult> {
        let params = ToolCallParams::new(name, arguments).with_context(context);
        self.request(methods::TOOLS_CALL, &params).await
    }

    /// `hooks/before_tool`.
    pub async fn before_tool(&self, tool: &str, input: Value) -> Result<BeforeToolResult> {
        let params = BeforeToolParams {
            tool: tool.to_string(),
            input,
        };
        self.request(methods::HOOKS_BEFORE_TOOL, &params).await
    }

    /// Stop the plugin gracefully: send `shutdown` and wait up to
    /// [`SHUTDOWN_GRACE`] for the answer, close its stdin once the lines
    /// already queued are written (waiting up to [`SHUTDOWN_GRACE`]), wait
    /// up to [`SHUTDOWN_GRACE`] for the process to exit, then kill it.
    /// Later requests and notifications fail.
    pub async fn shutdown(&self) {
        let answered = if self.is_closed() {
            Ok(Value::Null)
        } else {
            self.call_with_timeout(methods::SHUTDOWN, Value::Null, SHUTDOWN_GRACE)
                .await
        };
        if let Err(e) = answered {
            tracing::debug!(plugin = %self.name(), "shutdown request failed: {e}");
        }
        // Closing the plugin's stdin lets a plugin blocked reading it see
        // end-of-file and exit.
        let outbox = lock(&self.shared.outbox).take();
        if let Some(outbox) = outbox {
            let (done, closed) = oneshot::channel();
            let close = async move {
                if outbox.send(Outgoing::Close(done)).await.is_ok() {
                    let _ = closed.await;
                }
            };
            if tokio::time::timeout(SHUTDOWN_GRACE, close).await.is_err() {
                // The writer is stuck on a plugin that stopped reading:
                // stopping the task drops the writer, which closes stdin.
                if let Some(task) = lock(&self.writer_task).take() {
                    task.abort();
                }
            }
        }
        if let Some(mut child) = self.child.lock().await.take() {
            match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::debug!(plugin = %self.name(), "plugin exited: {status}");
                }
                _ => {
                    tracing::debug!(plugin = %self.name(), "killing plugin");
                    let _ = child.kill().await;
                }
            }
        }
    }
}

/// Resolve the program of a plugin command against its working directory:
/// a relative path containing a separator (`./plugin`, `bin/plugin`) is
/// joined to `cwd`; absolute paths and bare names (looked up on `PATH`) are
/// returned unchanged.
#[must_use]
pub fn resolve_program(program: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(program);
    let has_separator = program.contains('/') || program.contains(std::path::MAIN_SEPARATOR);
    if path.is_relative() && has_separator {
        cwd.join(path)
    } else {
        path.to_path_buf()
    }
}

async fn read_loop<R: AsyncRead + Unpin>(shared: Arc<Shared>, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match Message::parse(&line) {
                    Ok(message) => dispatch(&shared, message),
                    Err(e) => tracing::warn!(
                        plugin = %shared.name,
                        "ignoring invalid line from plugin: {e}"
                    ),
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(plugin = %shared.name, "cannot read from plugin: {e}");
                break;
            }
        }
    }
    shared.close("closed its output");
}

fn dispatch(shared: &Arc<Shared>, message: Message) {
    match message {
        Message::Response(resp) => {
            let Some(crate::protocol::RequestId::Number(id)) = resp.id else {
                tracing::warn!(plugin = %shared.name, "response with unexpected id: {:?}", resp.id);
                return;
            };
            let Some(tx) = shared.state().pending.remove(&id) else {
                tracing::debug!(plugin = %shared.name, "late or unknown response {id}");
                return;
            };
            let outcome = match (resp.error, resp.result) {
                (Some(err), _) => Err(Error::plugin(format!(
                    "plugin `{}` returned an error: {err}",
                    shared.name
                ))),
                (None, result) => Ok(result.unwrap_or(Value::Null)),
            };
            let _ = tx.send(outcome);
        }
        Message::Notification(n) => {
            if n.method == methods::LOG || n.method == methods::MCP_LOG {
                match n.params.map(serde_json::from_value::<LogParams>) {
                    Some(Ok(p)) => log_line(&shared.name, &p),
                    _ => tracing::debug!(plugin = %shared.name, "malformed log notification"),
                }
            } else {
                tracing::debug!(plugin = %shared.name, "ignoring notification `{}`", n.method);
            }
        }
        Message::Request(req) => {
            // Requests from the plugin: only `ping` is supported.
            let response = if req.method == methods::PING {
                Response::success(req.id, serde_json::json!({}))
            } else {
                Response::failure(Some(req.id), RpcError::method_not_found(&req.method))
            };
            // Answer on a separate task: the reader must keep draining the
            // plugin's stdout even while the host is blocked writing.
            let shared = Arc::clone(shared);
            tokio::spawn(async move {
                if let Err(e) = shared.write_message(&Message::Response(response)).await {
                    tracing::debug!(plugin = %shared.name, "cannot answer plugin request: {e}");
                }
            });
        }
    }
}

fn log_line(plugin: &str, params: &LogParams) {
    let text = params.text();
    match params.level.to_ascii_lowercase().as_str() {
        "error" | "critical" | "alert" | "emergency" => tracing::error!(plugin, "{text}"),
        "warn" | "warning" => tracing::warn!(plugin, "{text}"),
        "debug" => tracing::debug!(plugin, "{text}"),
        "trace" => tracing::trace!(plugin, "{text}"),
        _ => tracing::info!(plugin, "{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    /// A scripted peer: answers each request with `respond(method, params)`,
    /// or stays silent when it returns `None`.
    fn fake_plugin<F>(respond: F) -> PluginProcess
    where
        F: Fn(&str, &Value) -> Option<Value> + Send + 'static,
    {
        let (host_side, plugin_side) = duplex(64 * 1024);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (plugin_read, mut plugin_write) = tokio::io::split(plugin_side);
        tokio::spawn(async move {
            let mut lines = BufReader::new(plugin_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(Message::Request(req)) = Message::parse(&line) else {
                    continue;
                };
                let params = req.params.clone().unwrap_or(Value::Null);
                if let Some(result) = respond(&req.method, &params) {
                    let out = if let Some(msg) = result.get("__error").and_then(Value::as_str) {
                        Response::failure(Some(req.id), RpcError::new(-1, msg))
                    } else {
                        Response::success(req.id, result)
                    };
                    let mut text = Message::Response(out).to_line().unwrap();
                    text.push('\n');
                    // Interleave a notification to check it does not disturb routing.
                    plugin_write
                        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"log\",\"params\":{\"level\":\"info\",\"message\":\"hi\"}}\n")
                        .await
                        .unwrap();
                    plugin_write.write_all(text.as_bytes()).await.unwrap();
                    plugin_write.flush().await.unwrap();
                }
            }
        });
        PluginProcess::from_streams("fake", host_read, host_write)
    }

    #[tokio::test]
    async fn routes_responses_and_errors() {
        let p = fake_plugin(|method, params| match method {
            "echo" => Some(params.clone()),
            "fail" => Some(json!({"__error": "boom"})),
            _ => None,
        });
        let (a, b) = tokio::join!(p.call("echo", json!({"n": 1})), p.call("echo", json!(2)));
        assert_eq!(a.unwrap(), json!({"n": 1}));
        assert_eq!(b.unwrap(), json!(2));
        let err = p.call("fail", Value::Null).await.unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Plugin);
        assert!(err.message.contains("boom"));
    }

    #[tokio::test]
    async fn aborted_caller_leaves_no_pending_entry() {
        let p = Arc::new(fake_plugin(|_, _| None));
        let caller = {
            let p = Arc::clone(&p);
            tokio::spawn(async move { p.call("silent", Value::Null).await })
        };
        // Wait until the request is registered and in flight.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while p.shared.state().pending.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "call never registered"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(p.shared.state().pending.is_empty());
        assert!(
            !p.is_closed(),
            "dropping a caller must not break the connection"
        );
    }

    #[tokio::test]
    async fn times_out_silent_plugin() {
        let p = fake_plugin(|_, _| None);
        let err = p
            .call_with_timeout("silent", Value::Null, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Plugin);
        assert!(err.message.contains("did not answer"));
    }

    #[tokio::test]
    async fn write_to_stalled_plugin_times_out() {
        // The peer never reads, so the 64-byte pipe fills up on the first write.
        let (host_side, _plugin_side) = duplex(64);
        let (r, w) = tokio::io::split(host_side);
        let p = PluginProcess::from_streams("stalled", r, w);
        let started = std::time::Instant::now();
        let err = p
            .call_with_timeout(
                "big",
                json!({"blob": "x".repeat(8192)}),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("did not answer"));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(p.shared.state().pending.is_empty());
        // The writer task still owns the line, so the stream stays usable;
        // later calls time out too instead of hanging.
        assert!(!p.is_closed());
        let err = p
            .call_with_timeout("next", Value::Null, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.message.contains("did not answer"));
    }

    /// Plugin side of a slow pipe: reads a few bytes at a time (pausing
    /// while `slow` is set), checks every line is a whole JSON-RPC message,
    /// echoes requests' params back and reports each message it received.
    fn slow_reader(
        plugin_side: tokio::io::DuplexStream,
        slow: Arc<std::sync::atomic::AtomicBool>,
        received: Arc<std::sync::atomic::AtomicUsize>,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Message> {
        use std::sync::atomic::Ordering;
        use tokio::io::AsyncReadExt;
        let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut plugin_read, mut plugin_write) = tokio::io::split(plugin_side);
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8];
            loop {
                let n = plugin_read.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                received.fetch_add(n, Ordering::SeqCst);
                buf.extend_from_slice(&chunk[..n]);
                while let Some(end) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=end).collect();
                    let text = std::str::from_utf8(&line).unwrap();
                    let message = Message::parse(text.trim_end())
                        .unwrap_or_else(|e| panic!("partial or corrupt line {text:?}: {e}"));
                    if let Message::Request(req) = &message {
                        let result = req.params.clone().unwrap_or(Value::Null);
                        let out = Response::success(req.id.clone(), result);
                        let mut reply = Message::Response(out).to_line().unwrap();
                        reply.push('\n');
                        plugin_write.write_all(reply.as_bytes()).await.unwrap();
                    }
                    let _ = seen_tx.send(message);
                }
                if slow.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        });
        seen_rx
    }

    #[tokio::test]
    async fn cancelled_request_never_leaves_a_partial_line() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        // A 16-byte pipe drained 8 bytes every 5 ms keeps the writer busy.
        let (host_side, plugin_side) = duplex(16);
        let (r, w) = tokio::io::split(host_side);
        let p = PluginProcess::from_streams("slow", r, w);
        let slow = Arc::new(AtomicBool::new(true));
        let received = Arc::new(AtomicUsize::new(0));
        let mut seen = slow_reader(plugin_side, Arc::clone(&slow), Arc::clone(&received));

        let blob = "x".repeat(4096);
        let cancelled = tokio::time::timeout(
            Duration::from_millis(50),
            p.call("big", json!({ "blob": blob })),
        )
        .await;
        assert!(cancelled.is_err(), "the call must be cancelled mid-write");
        let sent = received.load(Ordering::SeqCst);
        assert!(
            sent > 0 && sent < blob.len(),
            "cancelled after {sent} bytes"
        );
        assert!(p.shared.state().pending.is_empty());
        assert!(!p.is_closed(), "cancelling a caller must not break framing");

        // The plugin gets the whole cancelled line, then the next request,
        // which is answered normally.
        slow.store(false, Ordering::SeqCst);
        let answer = p
            .call_with_timeout("small", json!({ "n": 1 }), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(answer, json!({ "n": 1 }));
        let methods: Vec<String> = [seen.recv().await, seen.recv().await]
            .into_iter()
            .map(|m| match m {
                Some(Message::Request(req)) => req.method,
                other => panic!("expected a request, got {other:?}"),
            })
            .collect();
        assert_eq!(methods, ["big", "small"]);
    }

    #[tokio::test]
    async fn caller_cancelled_before_its_turn_sends_nothing() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let (host_side, plugin_side) = duplex(16);
        let (r, w) = tokio::io::split(host_side);
        let p = PluginProcess::from_streams("queued", r, w);
        let slow = Arc::new(AtomicBool::new(true));
        let received = Arc::new(AtomicUsize::new(0));
        let mut seen = slow_reader(plugin_side, Arc::clone(&slow), Arc::clone(&received));

        // `first` (polled first, so queued first) keeps the writer busy
        // while `second` waits in the queue until its caller gives up.
        let first = tokio::time::timeout(
            Duration::from_secs(10),
            p.notify("first", json!({ "blob": "x".repeat(2048) })),
        );
        let second = async {
            let second =
                tokio::time::timeout(Duration::from_millis(30), p.call("second", Value::Null))
                    .await;
            slow.store(false, Ordering::SeqCst);
            second
        };
        let (first, second) = tokio::join!(first, second);
        assert!(second.is_err());
        first.unwrap().unwrap();
        let answer = p
            .call_with_timeout("third", Value::Null, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(answer, Value::Null);
        let mut methods = Vec::new();
        for _ in 0..2 {
            match seen.recv().await.unwrap() {
                Message::Request(req) => methods.push(req.method),
                Message::Notification(n) => methods.push(n.method),
                Message::Response(_) => panic!("unexpected response"),
            }
        }
        assert_eq!(methods, ["first", "third"]);
    }

    #[tokio::test]
    async fn writes_fail_after_shutdown() {
        let (host_side, plugin_side) = duplex(1024);
        let (r, w) = tokio::io::split(host_side);
        let p = PluginProcess::from_streams("bye", r, w);
        let (plugin_read, mut plugin_write) = tokio::io::split(plugin_side);
        let (eof_tx, eof_rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(plugin_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(Message::Request(req)) = Message::parse(&line) {
                    let out = Response::success(req.id, json!({}));
                    let mut reply = Message::Response(out).to_line().unwrap();
                    reply.push('\n');
                    plugin_write.write_all(reply.as_bytes()).await.unwrap();
                }
            }
            // Keep the host's reader open: only stdin is closed.
            let _ = eof_tx.send(());
            std::future::pending::<()>().await;
        });

        let started = std::time::Instant::now();
        p.shutdown().await;
        assert!(started.elapsed() < SHUTDOWN_GRACE);
        // The plugin saw end-of-file on its stdin.
        tokio::time::timeout(Duration::from_secs(1), eof_rx)
            .await
            .unwrap()
            .unwrap();
        let limit = Duration::from_secs(1);
        let notified = tokio::time::timeout(limit, p.notify("late", Value::Null)).await;
        assert!(notified.expect("notify must not hang").is_err());
        let called = tokio::time::timeout(limit, p.call("late", Value::Null)).await;
        let err = called.expect("call must not hang").unwrap_err();
        assert!(err.message.contains("closed"));
    }

    #[tokio::test]
    async fn write_failure_breaks_the_client() {
        // The reader stays open, so only the writer can notice the failure.
        let (r, _keep_reader) = duplex(64);
        let (w, plugin_stdin) = duplex(1024);
        let p = PluginProcess::from_streams("broken", r, w);
        let waiting = p.call("waiting", Value::Null);
        let breaker = async {
            // Let `waiting` be written, then make the next write fail.
            while p.shared.state().pending.is_empty() {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(plugin_stdin);
            p.notify("next", Value::Null).await
        };
        let (waiting, next) = tokio::join!(waiting, breaker);
        assert!(next.unwrap_err().message.contains("cannot write"));
        assert!(
            waiting
                .unwrap_err()
                .message
                .contains("stopped accepting input")
        );
        assert!(p.is_closed());
        let later = tokio::time::timeout(Duration::from_secs(1), p.notify("later", Value::Null));
        assert!(later.await.expect("must not hang").is_err());
    }

    #[tokio::test]
    async fn closed_stream_fails_pending_calls() {
        let (host_side, plugin_side) = duplex(1024);
        let (r, w) = tokio::io::split(host_side);
        let p = PluginProcess::from_streams("gone", r, w);
        let call = p.call("x", Value::Null);
        let closer = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(plugin_side);
        };
        let (res, ()) = tokio::join!(call, closer);
        assert!(res.is_err());
        assert!(p.is_closed());
        assert!(p.call("y", Value::Null).await.is_err());
    }

    #[test]
    fn programs_resolve_against_cwd() {
        let cwd = Path::new("/plugins/echo");
        assert_eq!(
            resolve_program("./bin/p", cwd),
            Path::new("/plugins/echo").join("./bin/p")
        );
        assert_eq!(resolve_program("python3", cwd), PathBuf::from("python3"));
        let abs = std::env::temp_dir().join("p");
        assert_eq!(resolve_program(abs.to_str().unwrap(), cwd), abs);
    }

    #[tokio::test]
    async fn missing_cwd_is_reported() {
        let cfg = PluginConfig {
            name: "nowhere".into(),
            command: vec!["x".into()],
            env: Default::default(),
            cwd: Some(std::env::temp_dir().join("vibe-plugins-no-such-dir-4242")),
            capabilities: Vec::new(),
            required: true,
        };
        let err = PluginProcess::spawn(&cfg).unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Plugin);
        assert!(err.message.contains("nowhere"));
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        let cfg = PluginConfig {
            name: "empty".into(),
            command: vec![],
            env: Default::default(),
            cwd: None,
            capabilities: Vec::new(),
            required: true,
        };
        let err = PluginProcess::spawn(&cfg).unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Plugin);
    }
}
