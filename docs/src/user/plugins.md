# Plugins

A **plugin** is a separate program that gives agents new tools, adds agents, or vetoes tool
calls. It can be written in any language: Vibe Factory starts it, talks to it over its
standard input and output, and stops it at the end of the run. Existing Model Context
Protocol (MCP) servers that run over stdio work as tool plugins without changes. The wire
contract is described in [Plugin protocol](../design/plugin-protocol.md).

| A plugin can provide | Shown to agents as |
|----------------------|--------------------|
| tools | extra tools, offered to every agent whose tool selection is `all` or names them |
| agents | extra roles, or replacements for built-in ones |
| a `before_tool` hook | a policy that sees every tool call and may block it |

## Declaring a plugin in the configuration

Add a `[[plugins]]` entry to `.vibe/config.toml` for each plugin:

```toml
[[plugins]]
name = "jira"
command = ["jira-vibe-plugin", "--project", "PAY"]
env = { JIRA_URL = "https://jira.example.com", JIRA_TOKEN_FILE = "/run/secrets/jira" }
cwd = "tools/jira"
capabilities = ["tools"]
required = false
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `name` | string | required | unique name, used in logs and listings |
| `command` | list of strings | required | program followed by its arguments |
| `env` | table of strings | none | extra environment variables; the rest of the environment is inherited |
| `cwd` | path | the directory `vibe` runs in (normally the project root) | working directory of the process |
| `capabilities` | list of `tools`, `agents`, `hooks` | none | informational only: the plugin's handshake decides |
| `required` | bool | `false` | when `true`, a plugin that fails to start stops the run |

A program written as a relative path with a separator (`./bin/plugin`, `bin/plugin`) is
resolved against `cwd`. A bare name (`python3`, `jira-vibe-plugin`) is looked up on `PATH`.

## Installing a plugin from a manifest

A plugin can also ship its own manifest. Put it in a directory under `.vibe/plugins/`:

```text
.vibe/plugins/
  echo/
    vibe-plugin.toml
    bin/vibe-echo-plugin
```

```toml
# .vibe/plugins/echo/vibe-plugin.toml
name = "echo"
version = "0.1.0"
description = "Echoes its input"
command = ["./bin/vibe-echo-plugin"]
required = false
capabilities = ["tools", "agents", "hooks"]

[env]
ECHO_PREFIX = ">"
```

The keys are those of `[[plugins]]` plus `version` and `description`; there is no `cwd`,
because the plugin always runs in the manifest's directory. `capabilities` may also be
written as a table (`[capabilities]` with `tools = true`, …). An invalid manifest is skipped
with a warning naming the file.

**User-level plugins** live in the same layout under your configuration directory and are
available in every project:

| Platform | Directory |
|----------|-----------|
| Linux | `~/.config/vibe/plugins/<name>/vibe-plugin.toml` |
| macOS | `~/Library/Application Support/vibe/plugins/<name>/vibe-plugin.toml` |
| Windows | `%APPDATA%\vibe\plugins\<name>\vibe-plugin.toml` |

When the same name is declared in several places, the first one wins, in this order:
`[[plugins]]` in `.vibe/config.toml`, then `.vibe/plugins/`, then the user directory. This
lets a project pin or replace a plugin you installed globally.

## Using an MCP server as a plugin

Point a declaration at the server's stdio command. For example, with a hypothetical
filesystem server `mcp-fs-server` that serves one directory:

```toml
[[plugins]]
name = "fs"
command = ["mcp-fs-server", "--stdio", "--root", "."]
```

Vibe Factory performs the MCP handshake, lists the server's tools (following pagination) and
registers them under their own names. Things to know:

- MCP servers provide tools only; they never add agents or hooks.
- A tool is treated as read-only only if the server marks it with the `readOnlyHint`
  annotation. Other tools are treated as mutating and never run concurrently with other
  calls.
- The framework passes the task's workspace root and permissions in the request metadata,
  but MCP servers ignore it. The server works on whatever you configured it for: here
  `--root .` resolves against `cwd`, that is your project checkout, **not** the task's
  worktree. Give such servers the narrowest scope that works, and prefer read-only servers
  when agents run in isolated worktrees.
- A tool whose name matches a built-in tool replaces it (a warning is logged). Rename the
  server's tools, or avoid servers that shadow `read_file`, `bash` and friends.

## The echo example

The repository ships `vibe-echo-plugin`, a complete plugin used by the tests. It offers:

- a tool `echo` that returns its `text` argument (with `"workspace": true` it returns the
  workspace root it received instead);
- an agent with the custom role `echo_agent` that may only use `echo`;
- a `before_tool` hook that blocks any call to a tool named `forbidden`.

Build it and declare it:

```sh
cargo build --release -p vibe-plugins --bin vibe-echo-plugin
```

```toml
[[plugins]]
name = "echo"
command = ["./target/release/vibe-echo-plugin"]
```

You can also talk to it by hand to see the protocol:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":"1","host":{"name":"me","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown"}' \
  | ./target/release/vibe-echo-plugin
```

## Troubleshooting

**Where are the logs?** Everything a plugin writes to its standard error is logged by Vibe
Factory at debug level, prefixed with `[<plugin name>]`. Log notifications sent over the
protocol are logged at their own level. Enable debug logging to see both. A plugin must never
print diagnostics to standard output: that stream carries the protocol, and a stray line is
reported as `ignoring invalid line from plugin`.

**The plugin does not start.** The message names the program and the directory it was
started in:

```text
skipping plugin `jira`: Plugin: cannot start plugin `jira` (`jira-vibe-plugin` in `/repo`): No such file or directory
```

Check the program is on `PATH` or use a path relative to `cwd`, and that it is executable.

**Required or optional?** An optional plugin (the default) that fails to start is skipped
with a warning and the run continues without it. A plugin with `required = true` that fails
stops loading: every plugin already started is shut down and the run reports
``required plugin `x` failed to start: …``. Mark a plugin required when running without it
would be unsafe, typically a policy plugin whose hook blocks dangerous commands.

**Timeouts.** A tool call may take up to 600 seconds; every other request, including the
handshake, 60 seconds. A plugin that does not answer in time produces
``plugin `x` did not answer `tools/call` within 600s``, which the agent sees as a failed tool
call. At the end of the run each plugin gets 2 seconds to answer `shutdown` and 2 more to
exit before it is killed.

**Every tool call is blocked.** A plugin hook that crashed or stopped answering blocks the
calls it guards (`hook plugin unavailable: …`): hooks fail closed on purpose. Restart the run
after fixing the plugin, or remove it.

**Tools are missing.** The handshake is authoritative: a plugin that does not report the
`tools` capability is never asked for its tools, whatever `capabilities` says in your
configuration. Also check the agent's tool selection: roles with `read_only` or `read_write`
tools never see plugin tools unless you list them explicitly (see
[Customising agents](agents.md)).

**The same plugin twice.** A name declared twice is loaded once; the duplicate is ignored
with a warning.
