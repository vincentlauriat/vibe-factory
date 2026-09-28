# Plugin protocol

The **Vibe Plugin Protocol** (VPP) lets a program written in any language contribute tools,
agent specs and a `before_tool` hook to a run. It is JSON-RPC 2.0 over the plugin's stdin and
stdout, shaped like the Model Context Protocol (MCP) so that an MCP stdio server works
unchanged as a tool plugin ([ADR-005](adr/005-plugin-protocol.md)). The reference
implementation is `crates/vibe-plugins`: `protocol.rs` (wire types), `client.rs` (host side),
`server.rs` (plugin side), `remote.rs` (adapters), `host.rs`, `manifest.rs` and
`discovery.rs`.

## Framing

- One JSON object per line, UTF-8, terminated by `\n` (newline-delimited JSON). A message
  must not contain a raw newline.
- The host writes requests and notifications to the plugin's **stdin** and reads responses
  and notifications from its **stdout**.
- **stderr** is free-form diagnostics; the host logs each line at debug level as
  `[<plugin name>] <line>`. Never write diagnostics to stdout.
- Blank lines are ignored. An unparsable line from a plugin is logged and skipped.

## Envelope

| Kind | Shape | Rule |
|------|-------|------|
| Request | `{"jsonrpc":"2.0","id":N,"method":"…","params":{…}}` | has `method` and a non-null `id`; expects exactly one response |
| Notification | `{"jsonrpc":"2.0","method":"…","params":{…}}` | has `method`, no `id`; never answered |
| Response | `{"jsonrpc":"2.0","id":N,"result":…}` or `{…,"error":{"code","message","data?"}}` | exactly one of `result` and `error` |

`id` may be a number or a string; the host generates increasing integers starting at 1 and
only matches numeric ids. Responses may arrive in any order; the plugin may handle requests
concurrently. `params` is omitted when there are none (`shutdown`).

| Error code | Constant | Meaning |
|-----------|----------|---------|
| -32700 | `PARSE_ERROR` | invalid JSON received |
| -32600 | `INVALID_REQUEST` | not a valid request |
| -32601 | `METHOD_NOT_FOUND` | unknown method (mandatory answer for methods you do not implement) |
| -32602 | `INVALID_PARAMS` | bad parameters (also used by `PluginServer` for an unknown tool) |
| -32603 | `INTERNAL_ERROR` | internal failure |

An error response is reported by the host as
``plugin `<name>` returned an error: <message> (code <code>)``.

## Lifecycle

```text
host                                          plugin
 │  spawn command in cwd, env, piped stdio      │
 │── initialize ───────────────────────────────▶│   60 s
 │◀──────────── {name, version, capabilities} ──│
 │── notifications/initialized ────────────────▶│   (ignore it)
 │── tools/list  (if tools)   ─────────────────▶│   60 s, follows nextCursor
 │── agents/list (if agents)  ─────────────────▶│   60 s
 │        … run: tools/call (600 s), hooks/before_tool (60 s) …
 │── shutdown ─────────────────────────────────▶│   2 s
 │   close stdin, wait 2 s for exit, then kill  │
```

If the handshake or a listing fails, the process is shut down and the plugin counts as not
started. The host only calls `tools/*`, `agents/list` and `hooks/before_tool` when the
matching capability was advertised. All host writes go through one writer task that owns
the plugin's stdin and writes whole queued lines one at a time, so a caller that times out or
is cancelled never leaves a partial line: its line is written whole, or skipped if its turn had
not come yet. A failed write marks the connection dead and fails every pending and later
call fast; after `shutdown` closed stdin, writes fail with ``plugin `<name>` is closed``.

| Constant | Value | Applies to |
|----------|-------|-----------|
| `DEFAULT_TIMEOUT` | 60 s | every request except `tools/call` |
| `TOOL_CALL_TIMEOUT` | 600 s | `tools/call` |
| `SHUTDOWN_GRACE` | 2 s | answer to `shutdown`, then again for process exit |

A timeout surfaces as ``plugin `<name>` did not answer `<method>` within <n>s``.

## Methods

### initialize

The host sends the VPP fields and, for MCP servers, the MCP fields:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocol_version":"1",
  "host":{"name":"vibe-factory","version":"0.5.1"},
  "protocolVersion":"2025-06-18",
  "clientInfo":{"name":"vibe-factory","version":"0.5.1"},
  "capabilities":{}}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{"name":"echo","version":"0.1.0",
  "capabilities":{"tools":true,"agents":true,"hooks":true}}}
```

`protocol_version` is `PROTOCOL_VERSION` (`"1"`), `protocolVersion` is
`MCP_PROTOCOL_VERSION` (`"2025-06-18"`). Each capability flag is a boolean; when reading, an
object (`{"tools": {}}`) counts as `true` and `null` or absence as `false`. If `name` or
`version` is absent, the MCP `serverInfo` object is used.

### tools/list

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

```json
{"jsonrpc":"2.0","id":2,"result":{"tools":[
  {"name":"echo","description":"Return the given text unchanged.",
   "input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]},
   "mutating":false}]}}
```

| Descriptor field | Notes |
|------------------|-------|
| `name` | required; registered under this name, overriding a same-named tool |
| `description` | default `""` |
| `input_schema` | `inputSchema` accepted; default `{"type":"object"}` |
| `mutating` | optional; when absent, `!annotations.readOnlyHint`; when that is absent too, `true` |

If the result carries `nextCursor`, the host requests `{"cursor": "<value>"}` again, up to
100 pages.

### tools/call

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
  "name":"echo","arguments":{"text":"hi"},
  "_meta":{"vibe":{"workspace_root":"/repo/.vibe/worktrees/add-oauth-6f1c3e0a",
                   "task_id":"6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19","agent":"coder",
                   "permissions":{"read":true,"write":true,"execute":true,"network":false}}}}}
```

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hi"}],"is_error":false}}
```

`_meta.vibe` is the `ToolCallContext` built from the agent's `ToolContext`: the workspace
root the tool must stay inside, the task id (omitted when there is none), the calling
agent's role name and its permission flags. A plugin tool that touches files must resolve
paths against `workspace_root` and refuse to leave it; the host cannot enforce containment
inside another process. MCP servers ignore `_meta`.

The result's text blocks are joined with newlines; any non-text block becomes
`[<type> content omitted]`. `is_error` (or `isError`) turns the result into an error the
model sees. A tool failure should be a result with `is_error: true`, not a JSON-RPC error;
`PluginServer` does this for handlers that return `Err`.

### agents/list

```json
{"jsonrpc":"2.0","id":4,"method":"agents/list","params":{}}
```

```json
{"jsonrpc":"2.0","id":4,"result":{"agents":[
  {"role":"echo_agent","system_prompt":"You repeat what you are told using the `echo` tool.",
   "description":"Echoes its instructions","tools":{"named":["echo"]},"thinking":"low","max_steps":20}]}}
```

Each entry is the JSON form of `AgentSpec`. Only `role` and `system_prompt` are required;
`tools`, `model`, `thinking`, `max_steps`, `structured_output` and `max_tokens` take the
`AgentSpec` defaults. A built-in role name replaces the built-in agent.

### hooks/before_tool

```json
{"jsonrpc":"2.0","id":5,"method":"hooks/before_tool","params":{"tool":"forbidden","input":{}}}
```

```json
{"jsonrpc":"2.0","id":5,"result":{"decision":"abort","reason":"the echo plugin forbids this tool"}}
```

`decision` is `continue` (default) or `abort`; `reason` defaults to `"vetoed by plugin"`.
The hook sees every tool call of every agent. Remote hooks **fail closed**: if the plugin
times out, has exited or answers with an error, the call is aborted with `hook plugin
unavailable: …`. `before_phase`, `after_phase`, `after_tool` and `augment_prompt` have no
protocol method; they are available to in-process plugins only.

### shutdown and ping

`shutdown` has no params and is answered with `{}`; the plugin then exits. An MCP server
that does not know `shutdown` is tolerated: the host closes stdin and kills the process
after the grace period. `ping` is answered with `{}` by either side; it is the only request a
plugin may send to the host (any other gets `-32601`).

### Notifications

| Direction | Method | Params |
|-----------|--------|--------|
| host → plugin | `notifications/initialized` | none; sent once after `initialize`, may be ignored |
| plugin → host | `log` | `{"level": "error"\|"warn"\|"info"\|"debug"\|"trace", "message": "…"}` |
| plugin → host | `notifications/message` (MCP) | `{"level": "…", "data": …}`; `data` is used when `message` is absent |

Log levels map onto `tracing` levels (`warning` is accepted, `critical`, `alert` and
`emergency` map to error, anything else to info).

## MCP compatibility

| MCP field | Accepted as |
|-----------|-------------|
| `serverInfo` | fallback for `name` and `version` |
| capability objects `{"tools": {}}` | `true` |
| `inputSchema` | `input_schema` |
| `annotations.readOnlyHint` | inverse of `mutating` |
| `nextCursor` | pagination of `tools/list` |
| `isError` | `is_error` |
| `notifications/message` | `log` |

MCP servers expose neither agents nor VPP hooks, so only their tools are registered.

## Manifests and declarations

A plugin is declared inline in `.vibe/config.toml` (`[[plugins]]`, see
[Plugins](../user/plugins.md)) or by a manifest `vibe-plugin.toml`:

```toml
name = "echo"                            # required, unique
version = "0.1.0"
description = "Echoes its input"
command = ["./bin/vibe-echo-plugin"]     # required; program then arguments
required = false                         # true: a failure to start aborts loading
capabilities = ["tools", "agents", "hooks"]   # or [capabilities] tools = true …; informational

[env]
ECHO_PREFIX = ">"
```

The process runs in the manifest's directory. A program written as a relative path
(containing a separator, such as `./bin/p`) is resolved against that directory; a bare name
(`python3`) is looked up on `PATH`. Declared capabilities are informational: the
`initialize` answer is authoritative.

### Discovery precedence

1. Inline `[[plugins]]` entries of `.vibe/config.toml`.
2. `<project>/.vibe/plugins/*/vibe-plugin.toml`, sorted by directory name.
3. `<user config dir>/vibe/plugins/*/vibe-plugin.toml` (`dirs::config_dir()`, for example
   `~/.config/vibe/plugins` on Linux, `~/Library/Application Support/vibe/plugins` on macOS).

The first declaration of a name wins; later ones are skipped with a debug log. Invalid
manifests are logged and skipped. `discovery::plugin_configs(project, &config)` returns the
merged list.

## Host failure policy

`PluginHost::load` starts every plugin concurrently.

| Situation | Result |
|-----------|--------|
| optional plugin fails to spawn or handshake | warning `skipping plugin …`, run continues |
| `required = true` plugin fails | every started plugin is shut down; `ErrorKind::Plugin` error ``required plugin `x` failed to start: …`` |
| same name declared twice | the duplicate is ignored with a warning |
| tool call times out or errors | error result to the model; the run continues |
| hook plugin unavailable | the guarded tool call is aborted (fail closed) |

## Writing a plugin in Rust

Add `vibe-plugins`, `vibe-core`, `tokio` and `serde_json` as dependencies and build a
`PluginServer`:

```rust,ignore
use serde_json::{Value, json};
use vibe_core::{AgentRole, AgentSpec, HookDecision, ToolOutput, ToolSelection};
use vibe_plugins::{PluginServer, ToolCallContext};

#[tokio::main]
async fn main() {
    let server = PluginServer::new("todo-scan", "0.1.0");
    let log = server.logger();
    let server = server
        .tool_with_context(
            "count_todos",
            "Count TODO markers in a file of the workspace.",
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            move |args: Value, ctx: Option<ToolCallContext>| {
                let log = log.clone();
                async move {
                    let root = ctx.map(|c| c.workspace_root).unwrap_or_else(|| ".".into());
                    let path = root.join(args["path"].as_str().unwrap_or_default());
                    let text = tokio::fs::read_to_string(&path).await?;
                    log.log("debug", format!("scanned {}", path.display()));
                    Ok(ToolOutput::ok(text.matches("TODO").count().to_string()))
                }
            },
        )
        .agent(
            AgentSpec::new(AgentRole::Custom("todo_auditor".into()), "List the TODOs of {{task_title}}.")
                .with_tools(ToolSelection::Named(vec!["count_todos".into()])),
        )
        .before_tool(|tool, _input| async move {
            if tool == "bash" { HookDecision::Abort("todo-scan forbids the shell".into()) } else { HookDecision::Continue }
        });
    let code = if server.run_stdio().await.is_ok() { 0 } else { 1 };
    std::process::exit(code);
}
```

The example joins paths without a containment check for brevity; a real tool must reject
`..` and absolute paths. Notes:

- `tool` / `tool_with` / `tool_with_context` register a handler; an `Err` becomes a result
  with `is_error: true`. Use `tool_with(ToolDescriptor { mutating: Some(false), .. })` to let
  the host run the tool concurrently.
- Capabilities are derived from what you registered.
- Requests run concurrently; `shutdown` or end of input stops the server.
- Call `std::process::exit` after `run_stdio` returns: Tokio reads stdin on a blocking
  thread that would otherwise keep the runtime alive. `vibe-echo-plugin` does this.
- `PluginServer::serve(reader, writer)` and `PluginServer::handle(method, params)` let you
  test the plugin without a process.

Build it, then add `.vibe/plugins/todo-scan/vibe-plugin.toml` with
`command = ["/path/to/target/release/todo-scan"]`.

## Writing a plugin in Python

Any language that can read lines and write JSON works. This sketch implements the contract
with one read-only tool and a hook:

```python
#!/usr/bin/env python3
import json, sys

TOOLS = [{"name": "upper", "description": "Uppercase a text.", "mutating": False,
          "input_schema": {"type": "object", "properties": {"text": {"type": "string"}},
                           "required": ["text"]}}]

def handle(method, params):
    if method == "initialize":
        return {"name": "upper", "version": "0.1.0",
                "capabilities": {"tools": True, "agents": False, "hooks": True}}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        text = params.get("arguments", {}).get("text", "")
        return {"content": [{"type": "text", "text": text.upper()}], "is_error": False}
    if method == "hooks/before_tool":
        if params.get("tool") == "bash" and "rm -rf" in json.dumps(params.get("input")):
            return {"decision": "abort", "reason": "no recursive deletes"}
        return {"decision": "continue"}
    if method in ("ping", "shutdown"):
        return {}
    raise KeyError(method)

for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    if "id" not in msg or "method" not in msg:
        continue                                   # notification or stray response
    try:
        reply = {"jsonrpc": "2.0", "id": msg["id"], "result": handle(msg["method"], msg.get("params") or {})}
    except KeyError:
        reply = {"jsonrpc": "2.0", "id": msg["id"],
                 "error": {"code": -32601, "message": "method not found: " + msg["method"]}}
    print(json.dumps(reply), flush=True)
    if msg["method"] == "shutdown":
        break
```

Declare it with `command = ["python3", "upper.py"]` in its manifest directory.
