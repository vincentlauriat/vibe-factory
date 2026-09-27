# Security model

Vibe Factory lets language models edit files and run commands in a developer's repository.
The security model answers one question: *what is the worst a confused or manipulated
model can do, and how do we bound it?* It is built in layers; each layer is independent
and any one of them can be tightened without touching the others.

```text
 ┌───────────────────────────────────────────────────────────┐
 │ 5. Isolation      git worktree (default) / plugin sandbox │
 │ 4. Hooks          before_tool veto, audit, prompt rules   │
 │ 3. Shell policy   parser + blocked programs + validators  │
 │ 2. Permissions    read / write / execute / network        │
 │ 1. Path containment   every path inside the workspace     │
 └───────────────────────────────────────────────────────────┘
```

## 1. Path containment (`vibe-core`)

`ToolContext::resolve_path` is the single entry point for paths. It joins relative paths
to the workspace root, normalises `.` and `..` lexically, canonicalises the longest
existing prefix (resolving symbolic links), re-appends the non-existing suffix and checks
that the result starts with the canonical workspace root (or one of
`Permissions::extra_read_paths`).

Two subtleties are handled explicitly:

- a **dangling symbolic link** cannot be canonicalised; the path is denied, because its
  eventual target is unknown and a write would create a file at that target;
- the **writing tools** (`write_file`, `edit_file`, the `bash` working directory) walk
  every component with `symlink_metadata` and refuse any symbolic link, even one that
  points inside the workspace, so that a later retarget cannot redirect a write.

`extra_read_paths` are honoured for reading only; the writing tools require the canonical
path to be inside the workspace root proper.

## 2. Permissions (`vibe-core`)

`Permissions { read, write, execute, network, extra_read_paths }` travel with every
`ToolContext`. The built-in tools check them before doing anything. Presets:
`read_only()`, `local()` (default: everything but network) and `all()`. The pipeline
derives them from `SecurityConfig` and from the agent's `ToolSelection`: a read-only agent
never receives a writing tool in the first place, and even if it did, the permission check
would refuse.

## 3. Shell policy (`vibe-tools`)

The `bash` tool hands the command line to `SecurityPolicy::validate_with_network` before
spawning anything. The policy is a **denylist with per-command validators**, optionally
narrowed by an allowlist ([ADR-004](adr/004-shell-policy.md)).

### Parsing

`command_parser::parse_command` splits on `|`, `||`, `&&`, `;`, `&` and newlines, descends
into `$(…)`, backticks and `( … )`, strips `env`/`time`/`nice`/`nohup`/`xargs`/`timeout`/
`exec`/`command`/`builtin` wrappers and `VAR=value` prefixes, and returns one
`CommandSegment { program, argv }` per command, up to `MAX_NESTING_DEPTH` (16). It fails
closed on `case`/`function` constructs, `env -S`, here-documents containing substitutions,
redirects without target and unbalanced quotes or parentheses. Program names that would be
computed at run time (`$`, backtick, `*`, `?`, `[`, `{`, `}`, `^`, `%`) are denied.

### Rules, in order

1. `BLOCKED_PROGRAMS` and `SecurityConfig::blocked_commands` → deny.
2. `SecurityConfig::allowed_commands` non-empty and program not in it (nor in
   `ALWAYS_ALLOWED`, the harmless builtins) → deny.
3. `NETWORK_PROGRAMS` and network not permitted → deny.
4. Per-command validators: `rm`, `chmod`, `git`, `kill`, `pkill`/`killall`, the shells
   (`-c` validated recursively; stdin-fed shells denied), `eval`, `find -exec`,
   database clients, `dropdb`/`dropuser`. The exact rules are listed in the
   [user guide](../user/security.md).

Every denial is a sentence returned to the model as an error tool result
(`ToolOutput::error`, `metadata.denied = true`), never a framework error, so the agent can
adapt instead of crashing the run.

### Execution

Commands run through `sh -c` (Unix) or `cmd /C` (Windows) in the workspace, with stdin
closed, `GIT_TERMINAL_PROMPT=0` and pagers disabled. A timeout kills the whole process
group (`taskkill /T /F` on Windows). Output is merged, capped at 30 000 characters
keeping head and tail, and ends with the exit code.

## 4. Hooks (`vibe-core`, plugins)

`Hook::before_tool` runs before every tool call with the `ToolContext`, tool name and
input; an `Abort(reason)` becomes an error tool result. `after_tool` sees every output.
`augment_prompt` injects project rules into the system prompt. Remote hooks (plugins)
**fail closed**: if the plugin cannot answer, the call is aborted. This is where teams put
policies the static layer cannot express.

## 5. Isolation (`vibe-workspace`, plugins)

The default workspace is a git worktree on its own branch
([ADR-003](adr/003-git-worktrees.md)); the project checkout is never modified until the
merge step, which refuses to run on a dirty tree. Stronger isolation (containers, VMs,
remote sandboxes) is a `WorkspaceProvider` plugin.

## Secrets

Provider layers scrub `sk-…`, `Bearer …`, `token=`, `api_key=` and `access_token=` from
every logged message (`vibe_providers::scrub_secrets`). API keys are read from environment
variables by default (`api_key_env`); inline keys in `config.toml` are supported but
discouraged. Plugins receive only the environment you declare in their configuration.

## Known limits

- The shell policy is defence in depth, **not a sandbox**: interpreters (`python -c`,
  `node -e`, `perl -e`) and build tools that run arbitrary scripts are allowed.
- On Windows the command is parsed with POSIX rules before being handed to `cmd /C`;
  `cmd`-specific tricks beyond `^` and `%` escapes may not be recognised.
- Prompt injection through repository content (a README telling the agent to run
  something) is mitigated by the same layers, not by detection.

Report weaknesses privately to the maintainers before opening a public issue.
