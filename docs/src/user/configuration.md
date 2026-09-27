# Configuration

A project is configured by one file, `.vibe/config.toml`, written by `vibe init`. Every key
is optional: a missing key takes the default listed below, and a missing file means "all
defaults". This page is the complete reference, generated from the `VibeConfig` type in
`crates/vibe-core/src/config.rs`.

```sh
vibe config path             # where the file is
vibe config show             # the effective configuration (file + defaults)
vibe config show --default   # the built-in defaults only
```

## Top-level keys

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `default_provider` | string | `"anthropic"` | provider name used when a model reference has no `provider/` prefix |
| `default_model` | string | `"anthropic/claude-sonnet-5"` | model used by every phase without a `[phases.<phase>]` entry |
| `base_branch` | string | unset | branch that task worktrees fork from and merge into; unset means the branch currently checked out |
| `[phases.<phase>]` | table | none | model and thinking level for one phase |
| `[providers.<name>]` | table | the three built-ins | model back-ends |
| `[pipeline]` | table | see below | pipeline tuning |
| `[security]` | table | see below | tool security policy |
| `[[plugins]]` | array of tables | none | out-of-process plugins to start |

Unknown keys are ignored by the TOML reader, so check spelling with `vibe config show`: a
key that does not appear there had no effect.

## Models and thinking per phase

The phases are `assess`, `spec`, `plan`, `build`, `qa`, `fix` and `merge`.

```toml
[phases.plan]
model = "anthropic/claude-opus-5"   # required in the table
thinking = "high"                    # optional: overrides every agent of the phase
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `model` | string, required | none | `provider/model`, or a bare model on `default_provider`, or `provider/` for that provider's default model |
| `thinking` | `off`, `low`, `medium`, `high`, `max` | unset | when set, thinking level of **every** agent run during this phase |

How a model is chosen for an agent:

1. an agent that pins a model (`model = "provider/model"` in `.vibe/agents/<role>.toml`)
   uses it;
2. otherwise `[phases.<phase>].model`;
3. otherwise `default_model`.

How a thinking level is chosen: an agent uses its own level (the built-in table in
[Customising agents](agents.md), or the `thinking` of an agent file) **unless** the current
phase has a `[phases.<phase>]` table that sets `thinking`, in which case that value applies
to every agent of the phase. A `[phases.build]` table that sets only `model` leaves each
agent's own thinking level untouched.

Model references and shorthands are described in
[Providers and models](providers.md#referring-to-models).

## `[providers.<name>]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `kind` | string, required | none | `anthropic`, `openai`, `ollama`, `mock`, or a kind registered by a plugin |
| `api_key` | string | unset | the key itself; wins over `api_key_env`; do not commit it |
| `api_key_env` | string | unset | environment variable holding the key; an empty value counts as missing |
| `base_url` | string | the vendor endpoint | gateway, proxy or self-hosted server |
| `default_model` | string | provider-specific | model used for `provider/` or `provider/default` |
| `extra` | table | empty | kind-specific settings (headers, auth mode, …) |

When the file contains **no** `[providers]` table at all, these three are used:

```toml
[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[providers.ollama]
kind = "openai"
base_url = "http://localhost:11434/v1"
default_model = "qwen2.5-coder"
```

As soon as you declare one `[providers.<name>]` table, the built-in list is replaced by the
tables you wrote. Declare every provider you reference. The `extra` keys and ready-made
recipes (Groq, OpenRouter, xAI, Mistral, mock) are in [Providers and models](providers.md).

## `[pipeline]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `max_qa_rounds` | integer | `3` | QA reviews per invocation of `vibe run`; when reached without approval, the run pauses with the task in `review` (minimum 1) |
| `max_subtask_attempts` | integer | `3` | attempts per subtask; the last one uses the `coder_recovery` agent; then the subtask is failed (minimum 1) |
| `max_parallel_subtasks` | integer | `3` | subtasks implemented at the same time (minimum 1) |
| `max_phase_retries` | integer | `2` | extra planner attempts when the plan is invalid (so 3 attempts by default) |
| `workspace` | string | `"git_worktree"` | `git_worktree`, `in_place`, or a workspace provider registered by a plugin |
| `auto_merge` | bool | `false` | merge after QA approval and required validations; otherwise stop in `ready` |
| `validation_commands` | list of strings | `[]` | mandatory shell checks before ready/merge; see below |
| `max_validation_fix_attempts` | integer | `2` | automatic validation fixes over the whole run, including resumes; `0` disables them |
| `merge_strategy` | `"manual"` or `"assisted"` | `"manual"` | `manual` reports conflicts for a human; `assisted` lets a model resolve conflict markers first |
| `isolate_subtasks` | bool | `true` | with `git_worktree`, one worktree per subtask attempt, integrated one at a time; see [Workspaces](workspaces.md#one-worktree-per-subtask-attempt) |
| `project_memory` | bool | `true` | keep lessons (pitfalls, conventions) in `.vibe/memory.jsonl` and recall the relevant ones in later tasks; see `vibe memory` |
| `approvals` | list of `"spec"`, `"plan"`, `"merge"` | `[]` | where the run waits for a human decision; see [Human approvals](#human-approvals) |
| `max_tokens` | integer | none | input plus output tokens of the whole run, including resumes; the run pauses when reached |
| `max_duration_secs` | integer | none | active time of the whole run in seconds, including resumes; the run pauses when reached |

What these limits do at run time is described in [The pipeline](../design/pipeline.md);
workspaces and merging in [Workspaces and merging](workspaces.md).

## Required validation commands

```toml
[pipeline]
validation_commands = ["cargo fmt --all -- --check", "cargo test --offline"]
max_validation_fix_attempts = 2
```

The default is `[]` for compatibility. Each configured command is mandatory and runs
sequentially from the task workspace root at the start of the merge phase, after normal
QA approval and before either marking the task ready or merging it automatically.
The pipeline calls the registered `bash` tool directly, without asking a model to interpret
its result. Tool hooks and the existing shell policy still apply. Custom `bash` tools must
return structured `exit_code: 0` metadata for success; tool registrations are trusted.
The timeout is `security.command_timeout_secs` (bounded by the shell tool to 1–600 seconds).

A nonzero exit or timeout sends the command, exit status and captured output to the QA fixer.
The fixer commits its changes, a fresh QA review runs, then **every required command runs
again** before integration. This applies even to the trivial complexity profile. A favorable
model verdict alone never satisfies a command check.

`max_validation_fix_attempts` defaults to 2 and limits automatic validation corrections over
the **whole run**, including resumes. Set it to 0 for manual correction only. Attempts are
reserved and saved before invoking the fixer, so an interrupted attempt still consumes one.
The existing QA review/fix limits also remain in effect. After exhausting the validation
budget, the run pauses with task status `review`. A denied/empty command, missing tool or
execution error without an exit status pauses immediately for a human; it is not sent to
an agent to work around the policy. Later validation commands do not run after a failure.

Results, output and metadata remain in `run.json` under `validations`; the persisted
`validation_fix_attempts` counter and `pending_validation_fix` index track correction and
recovery. Notes are also written to `progress.md`. Correct the workspace or configuration
and use `vibe run <task> --resume`: the gate rechecks all commands without granting fresh
automatic attempts. To grant more attempts explicitly, increase the configured limit.
Cancellation is checked between commands and before merge; a running command remains
bounded by its timeout.

Use checking commands, not formatters that rewrite sources. With `auto_merge = true`,
the commands also run on an isolated integration candidate that combines the latest target
branch and the task, **after** any assisted conflict resolution. Validation records include
`integration` (false for task checks, true for integration checks) and `workspace_root`.
A failed integration check pauses for human review without spending task-fixer attempts;
the target branch is not updated. Resuming rebuilds a candidate from the current target.

The git provider rejects tracked changes, new non-ignored files or commits left by a check
in the candidate. Build outputs must be ignored. It also refuses publication if the target
branch, current checkout, tracked files or guarded repository configuration changed during
validation. Only the already-tested commit can be fast-forwarded into the target.
Temporary candidate worktrees are removed after success or failure; the persisted output
and progress notes remain. This validates repository content, not production deployment.

With no validation commands the previous merge behavior is preserved. In-place mode has
no separate integration: checks run again on the project itself, with no rollback guarantee.
Third-party workspace providers must implement `merge_validated` to support gated automatic
integration; the default refuses it rather than falling back to an unchecked merge.
External manual `git merge` commands are outside this gate. Configuration is loaded by the
host, not accepted from an agent's QA report.

## Run budgets

```toml
[pipeline]
max_tokens = 2_000_000      # input + output tokens over the whole run
max_duration_secs = 3600    # one hour of active work
```

Both limits are off by default. They apply to a **run**, not to one invocation of
`vibe run`: `run.json` keeps the tokens (`usage`) and the active time (`active_ms`) of every
invocation, and a resumed run starts from those totals. Time spent paused or between
invocations is not counted.

Agents add the tokens of every model call to the budget as it happens. Once a limit is
reached no new model call starts: running sessions stop before their next step,
interrupted subtasks go back to pending, and the run **pauses** (exit code 2) at the phase
it was in, with a note in `progress.md` naming the limit. A running shell command is not
interrupted; it is bounded by its own timeout. Resuming without raising the limit pauses
again immediately and spends nothing. Raise the limit in the configuration, or for one
invocation with `vibe run <REF> --resume --max-tokens N --max-duration 2h`, to continue.

The token limit counts every model call of the run: the agents' calls and the ones made by
`merge_strategy = "assisted"` to resolve conflict markers.

## Human approvals

```toml
[pipeline]
approvals = ["plan", "merge"]
```

Each listed gate pauses the run (exit code 2, task `review`) until a human decides:

| Gate | The run stops | A rejection goes back to |
|------|---------------|--------------------------|
| `spec` | after the specification, before planning (only when the profile writes a spec) | the spec phase |
| `plan` | after the plan, before building | the planner |
| `merge` | after QA approval, before the merge phase (validation, integration or marking ready) | the fixer, through a QA report titled "Human review", then QA again |

```sh
vibe task show 3                       # read the spec, plan or QA report
vibe approve 3 --comment "ok"          # or:
vibe reject 3 --reason "Split the migration into its own subtask"
vibe run 3 --resume
```

The decision is stored in `run.json` (`approvals`, `pending_approval`, `rejection`) and
logged as an `approval_resolved` event. A rejection needs a reason: it is handed to the
agents that redo the work. Regenerating an artefact revokes its approval, so a new plan is
approved again, and a merge is approved again after any new build or fix. Resuming without
a decision pauses again at the same gate.

## `[workspace.container]`

Settings of the `container` workspace (`pipeline.workspace = "container"`). Only `image` is
required; unknown keys are refused.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `image` | string | required | image the commands run in |
| `runtime` | string | `"docker"` | `docker` or `podman`, or a path to one of them |
| `network` | string | `"none"` | `none`, `bridge` or a named network; attached only when `security.allow_network` is true |
| `mounts` | list of tables | `[]` | extra bind mounts: `source`, `target`, `read_only` (default true) |
| `cpus` | number | none | `--cpus` |
| `memory` | string | none | `--memory` and `--memory-swap`, such as `4g` |
| `pids_limit` | integer | `1024` | maximum number of processes |
| `tmp_size` | string | runtime default | size of the `/tmp` tmpfs |
| `user` | string | owner of the worktree (Unix) | `uid[:gid]` or `name[:group]` |
| `env` | list of names | `[]` | host environment variables passed through |
| `mount_git_metadata` | bool | `false` | mount the repository's git directory read-only (Unix only) |

See [Container workspace](workspaces.md#container-workspace) for what each setting allows.

## `[security]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `blocked_commands` | list of strings | `[]` | programs denied in addition to the built-in list |
| `allowed_commands` | list of strings | `[]` | when non-empty, only these programs (plus harmless builtins) may run |
| `command_timeout_secs` | integer | `120` | default timeout of a shell command; an agent may ask for up to 600 |
| `allow_network` | bool | `false` | grant the network permission (`curl`, `wget`, `ssh`, … and network-using tools) |
| `extra_read_paths` | list of paths | `[]` | directories outside the workspace that agents may read, never write |
| `web_allowed_domains` | list of strings | `[]` | domains (and subdomains) `web_fetch` may reach; empty means any public host |
| `search_url` | string | none | SearXNG-compatible search URL with `{query}`, e.g. `https://searx.example/search?q={query}&format=json`; registers `web_search` |

The rules behind these keys are in [Tools and security](security.md).

## `[[plugins]]`

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `name` | string, required | none | unique name for logs and `vibe plugins list` |
| `command` | list of strings | `[]` | program and arguments speaking the plugin protocol on stdio |
| `env` | table of strings | `{}` | extra environment variables |
| `cwd` | path | the project root | working directory of the process |
| `capabilities` | list of `tools`, `agents`, `hooks` | `[]` | informational; the handshake decides |
| `required` | bool | `false` | a failure to start stops the run instead of being skipped |

See [Plugins](plugins.md) for manifests under `.vibe/plugins/` and user-level plugins.

## A complete example

```toml
# .vibe/config.toml

# Models ------------------------------------------------------------------
default_provider = "anthropic"
default_model = "anthropic/claude-sonnet-5"   # every phase without its own table
# base_branch = "develop"                     # default: the branch checked out

# A cheap, fast assessment; a strong planner and reviewer; a local coder.
[phases.assess]
model = "anthropic/haiku"
thinking = "low"

[phases.plan]
model = "anthropic/opus"
thinking = "high"

[phases.build]
model = "ollama/qwen2.5-coder:32b"
thinking = "off"

[phases.qa]
model = "anthropic/opus"
thinking = "high"

# Providers: declaring one table replaces the built-in list, so list them all.
[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.ollama]
kind = "ollama"
default_model = "qwen2.5-coder:32b"

# Pipeline ----------------------------------------------------------------
[pipeline]
max_qa_rounds = 3           # reviews per `vibe run` before pausing for a human
max_subtask_attempts = 3    # the third attempt uses the recovery agent
max_parallel_subtasks = 2   # keep the local model responsive
max_phase_retries = 2       # extra planner attempts on an invalid plan
workspace = "git_worktree"  # or "in_place"
auto_merge = false          # stop in `ready` and let me merge
merge_strategy = "manual"
max_tokens = 3_000_000      # pause the run beyond this, resumes included
max_duration_secs = 7200    # and beyond two hours of active work

# Security ----------------------------------------------------------------
[security]
blocked_commands = ["docker", "terraform"]
allowed_commands = []                # empty: everything not blocked may run
command_timeout_secs = 300           # long test suites
allow_network = false
extra_read_paths = []

# Plugins -----------------------------------------------------------------
[[plugins]]
name = "policy"
command = ["./tools/vibe-policy-plugin"]
required = true                      # never run without the policy hook
```

Remember that in TOML a key written after a `[table]` header belongs to that table: put
top-level keys such as `base_branch` and `default_model` before the first header.

## Precedence

For each setting the first source that provides it wins:

1. **command-line flags** of `vibe run`: `--complexity`, `--provider`, `--model`,
   `--workspace`, `--auto-merge` (see [Command line reference](cli.md#vibe-run)).
   `--provider` and `--model` replace the model of every phase, but not a model pinned by an
   agent file;
2. **`.vibe/config.toml`**;
3. **built-in defaults** listed on this page.

Agent settings have their own layering, from lowest to highest: built-in agent, agents from
plugins, `.vibe/agents/*.toml` in file-name order; see [Customising agents](agents.md). The
`thinking` of a `[phases.<phase>]` table still overrides all of them for that phase.

## Environment variables

| Variable | Used for |
|----------|----------|
| `ANTHROPIC_API_KEY` | key of the built-in `anthropic` provider (`api_key_env`) |
| `OPENAI_API_KEY` | key of the built-in `openai` provider |
| any name in an `api_key_env` | key of that provider |
| `RUST_LOG` | log filter, for example `RUST_LOG=vibe_pipeline=debug,vibe_plugins=debug`; `-v` and `-vv` are shortcuts |
| `NO_COLOR` | disables colours, like `--no-color` |

Keys are read when a provider is built, at the start of each command. A provider whose key is
missing still loads and fails with `AuthFailed` on its first call.

## Changing values from the command line

`vibe config set` takes a dotted key and a value, edits the file in place and keeps the rest
of it, including comments. Values are parsed as TOML when they are valid TOML (numbers,
booleans, arrays, quoted strings) and as plain strings otherwise.

```sh
vibe config set pipeline.auto_merge true
vibe config set pipeline.max_parallel_subtasks 1
vibe config set default_model anthropic/claude-opus-5
vibe config set phases.plan.model openai/gpt-5
vibe config set phases.plan.thinking high
vibe config set security.blocked_commands '["docker", "kubectl"]'
vibe config set providers.ollama.base_url http://gpu-box:11434/v1
```

Missing tables are created. The whole configuration is parsed before the file is written,
so a value of the wrong type, or a `[phases.<phase>]` table without its required `model`, is
rejected (`` `phases.plan.thinking = high` makes the configuration invalid: … ``) and the
file is left unchanged: set `model` before `thinking`. The key name itself is not checked,
and a misspelled key is written and then ignored. Without a configuration file, `set` starts
from the defaults and creates it.

## Agents

Per-role settings (prompt, tools, pinned model, thinking, step and token budgets) do not
live in `config.toml` but in `.vibe/agents/*.toml`. `vibe agents export <role>` writes the
built-in definition of a role there as a starting point. The format is described in
[Customising agents](agents.md).
