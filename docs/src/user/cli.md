# Command line reference

The `vibe` binary drives everything: it creates tasks, runs them through the pipeline,
shows their state and inspects the configuration, agents and plugins. `vibe help <command>`
and `vibe <command> --help` print the same information as this page.

```text
vibe [GLOBAL OPTIONS] <COMMAND>

  init          create .vibe/config.toml
  task          add | list | show | discard
  run           run a task through the pipeline
  status        project summary
  config        show | path | set
  agents        list | show | export
  plugins       list | check
  doctor        check the environment and the configuration
  completions   print a shell completion script
```

## Global options

Global options may appear before or after the subcommand.

| Option | Effect |
|--------|--------|
| `-C, --project <DIR>` | start directory; the project is this directory (default: the current one) or its nearest parent that contains a `.vibe` directory; `vibe init` never searches parents |
| `-v, --verbose` | more logging on standard error: `-v` info, `-vv` debug, `-vvv` trace; `RUST_LOG`, when set, wins |
| `--json` | machine-readable output: one JSON document, or one JSON event per line for `vibe run` |
| `--no-color` | no colours; also the case when `NO_COLOR` is set or the output is not a terminal |
| `-h, --help` | help of the command |
| `-V, --version` | version of `vibe` |

## Task references

Commands that act on one task take a `<REF>`:

| Form | Example | Matches |
|------|---------|---------|
| number (up to six digits, leading zeros allowed) | `3`, `003` | the task numbered 3 |
| directory name | `003-add-oauth-login-github` | the task stored in `.vibe/tasks/003-…` |
| id prefix | `6f1c`, `6f1c3e0a-9b2d` | the task whose id starts with it; an ambiguous prefix is an error |

Numbers are tried first, then directory names, then id prefixes. A reference that matches
nothing is an error.

## `vibe init`

```sh
vibe init [--force]
```

Creates `.vibe/config.toml` with every key at its default value, under a short comment, in
the project directory (the current directory unless `-C` is given). It refuses to overwrite
an existing file unless `--force` is given. In a git repository it also adds `worktrees/`
and `tool-output/` to `.vibe/.gitignore`, keeping any lines already there; outside git it
warns that worktree isolation needs git. Run `vibe doctor` afterwards. The file is described in
[Configuration](configuration.md).

## `vibe task`

### `vibe task add`

```sh
vibe task add <TITLE> [-d <TEXT> | --description-file <PATH>] [--label <LABEL>]...
```

| Option | Effect |
|--------|--------|
| `-d, --description <TEXT>` | the description; `-` reads it from standard input |
| `--description-file <PATH>` | read the description from a file; `-` for standard input |
| `--label <LABEL>` | add a label; repeat for several |

`--description` and `--description-file` are mutually exclusive. The task is created in
`backlog` and gets the next number.

```sh
vibe task add "Add a --json flag to the export command" \
  -d "Print the exported records as a JSON array when --json is given." --label cli
git log -1 --format=%B | vibe task add "Follow up on the last commit" -d -
vibe task add "Migrate the settings page" --description-file notes/settings.md
```

### `vibe task list`

```sh
vibe task list [--status <STATUS>] [--all]
```

Lists tasks, newest first, with number, status, complexity and title. With `--json`, the
output is an array of `{"number": …, "task": {…}}` objects. `done` and
`cancelled` tasks are hidden unless `--all` is given. `--status` keeps one of `backlog`,
`planning`, `building`, `review`, `ready`, `done`, `failed`, `cancelled`.

### `vibe task show`

```sh
vibe task show <REF>
```

Shows the task: description, status and complexity, spec summary and requirements, plan
with subtask statuses, the latest QA verdict and issues, the state of the last run
(`run.json`) and the workspace branch and directory. The files it reads are listed in
[Persistence layout](../design/persistence.md).

### `vibe task discard`

```sh
vibe task discard <REF> [-y | --yes]
```

Removes the task's workspace (worktree and branch; nothing for `in_place`) and deletes the
task directory. It asks for confirmation on a terminal unless `--yes` is given; when standard
input is not a terminal it does not ask. Unmerged work on the task branch is lost.

## `vibe run`

```sh
vibe run <REF> [OPTIONS]
```

Runs the task through the pipeline and renders its progress live.

| Option | Effect |
|--------|--------|
| `--complexity <C>` | `trivial`, `simple`, `standard` or `complex`; skips the assessment |
| `--from <PHASE>` | start at this phase, reusing the persisted spec and plan |
| `--until <PHASE>` | pause (resumably) once this phase is done |
| `--dry-run` | only assess, specify and plan; same as `--until plan` |
| `--provider <NAME>` | use this provider for every phase, with its default model unless `--model` is given; `mock` works without any `[providers.mock]` table |
| `--model <PROVIDER/MODEL>` | use this model for every phase; a bare model name uses the default provider (or `--provider`) |
| `--workspace <NAME>` | `git_worktree`, `in_place`, or a workspace provider from a plugin |
| `--auto-merge` | merge automatically once QA approves |
| `--script <FILE>` | drive the `mock` provider with scripted responses from a JSON file; implies `--provider mock` and ignores `--provider` and `--model` |
| `--resume` | continue the last run of the task where it stopped |

Phases are `assess`, `spec`, `plan`, `build`, `qa`, `fix`, `merge`. These flags override the
configuration for this run only; nothing is written back. `--provider` and `--model` replace
`default_model` and the model of every `[phases.*]` table but keep their thinking levels;
an agent that pins its own model in `.vibe/agents/` keeps it. The script format is described
in the [Quick start](quickstart.md#try-it-with-no-api-key).

**Exit codes**

| Code | Meaning |
|------|---------|
| `0` | the task ended `ready` or `done`, or the run stopped where `--dry-run` or `--until` asked |
| `2` | the run paused for a human: task in `review` (QA did not approve, escalation, inconclusive review) |
| `1` | the run failed, or the command itself failed (unknown task, bad configuration, plugin or workspace error) |
| `130` | the run was cancelled (`Ctrl-C`, or a hook that aborted a phase) |

**Cancelling.** `Ctrl-C` cancels the run cooperatively: sessions stop before their next
model call, interrupted subtasks go back to pending, and `run.json` is saved as
`cancelled`. A second `Ctrl-C` exits immediately with code 130, leaving `run.json` as
`running`. Either way, `vibe run <REF> --resume`
continues later.

**Resuming.** `--resume` continues the last run from the phase recorded in `run.json`,
keeping its run id, profile, spec, plan and completed subtasks. The other options still
apply: `--from` overrides the phase to restart at, `--until` and `--dry-run` where to stop. A run that already finished
cannot be resumed; start a new one. `--from <PHASE>` instead starts a *new* run at that phase,
reusing the artefacts on disk; use it after fixing something by hand, for example
`vibe run 3 --from qa`. Resume semantics are detailed in
[The pipeline](../design/pipeline.md#resume).

```sh
vibe run 1                                   # full pipeline
vibe run 1 --dry-run                         # spec and plan only, then inspect them
vibe run 1 --resume                          # continue after the dry run
vibe run 1 --complexity trivial              # skip spec for a one-line fix
vibe run 1 --model anthropic/claude-opus-5   # one model for every phase
vibe run 1 --provider ollama                 # local model, provider's default
vibe run 1 --workspace in_place --auto-merge # no worktree, nothing to merge
vibe run 3 --from qa                         # review again after a manual fix
```

The run starts with a line naming the task, the build model and the workspace, renders the
events live (see the [Quick start](quickstart.md#5-run-it) for a sample), and ends with a
table of the phases of this invocation, the final status, duration and tokens, the branch,
worktree and task directory, and a hint for the next command.

With `--json`, `vibe run` prints the pipeline events as they happen, one JSON object per
line, in the `Envelope` format of `events.jsonl` (see
[Domain model](../design/domain-model.md#events)), then one last object of type `summary`
with `run_id`, `task_id`, `final_status`, `run_status`, `success`, `exit_code`,
`duration_ms`, `usage`, `phases`, `last_error`, `branch`, `worktree` and `task_dir`:

```sh
vibe run 1 --json | jq -r 'select(.event.type == "phase_finished") | "\(.event.phase): \(.event.summary)"'
```

## `vibe status`

```sh
vibe status
```

Project summary: the number of tasks per status, a table of recent runs (task, run status,
phase, last update) and the active task worktrees.

## `vibe config`

```sh
vibe config show [--default]
vibe config path
vibe config set <KEY> <VALUE>
```

| Subcommand | Effect |
|------------|--------|
| `show` | prints the effective configuration as TOML (file merged with defaults) |
| `show --default` | prints the built-in defaults |
| `path` | prints the path of `.vibe/config.toml` |
| `set <KEY> <VALUE>` | sets a dotted key; the value is a TOML literal (`true`, `3`, `["a", "b"]`) or else a plain string |

```sh
vibe config set pipeline.auto_merge true
vibe config set phases.plan.model anthropic/claude-opus-5
vibe config set security.blocked_commands '["docker"]'
```

## `vibe agents`

```sh
vibe agents list
vibe agents show <ROLE>
vibe agents export <ROLE> [--dir <DIR>] [--force]
```

| Subcommand | Effect |
|------------|--------|
| `list` | every agent with its tools, thinking level, model, source and description; the source is `builtin`, `override` (a built-in changed by `.vibe/agents/`) or `custom` |
| `show <ROLE>` | the system prompt of the agent after overrides (the whole `AgentSpec` with `--json`) |
| `export <ROLE>` | writes `<role>.toml` with every setting and `<role>.md` with the prompt into `.vibe/agents` (or `--dir`); refuses to overwrite either file without `--force` |

These commands read the built-in agents and `.vibe/agents/`; they do not start plugins, so
agents contributed by plugins are not listed. An invalid override file makes them fail with
the file name, which makes `vibe agents list` a quick check of your overrides. Roles and
override files are described in [Customising agents](agents.md).

## `vibe plugins`

```sh
vibe plugins list
vibe plugins check
```

`list` shows the declared and discovered plugins with name, source (`config` for
`[[plugins]]` entries, `manifest` for `.vibe/plugins/` and the user directory), whether they
are required, their declared capabilities and command. It starts nothing. `check` starts
every plugin, prints its version, the capabilities from its handshake and its tools, or the
error, then stops it; it exits with 1 when any plugin failed. Use it to debug a plugin before
a run. See [Plugins](plugins.md).

## `vibe doctor`

```sh
vibe doctor
```

Checks the environment and the configuration and prints one line per check, marked `✓`
(ok), `!` (warning) or `✗` (failure):

| Check | Fails when |
|-------|------------|
| `git` | `git --version` does not run |
| `config` | `.vibe/config.toml` does not parse (a missing file is only a warning) |
| `repository` | the project is not a git repository and the workspace is `git_worktree` |
| `providers` / `provider` | the provider table is invalid, a provider used by `default_provider`, `default_model` or a `[phases.*]` model is not declared, or a used provider has no key (an unused one only warns) |
| `plugin` | a plugin with `required = true` does not start (an optional one only warns) |
| `workspace` | `pipeline.workspace` is neither built in nor possibly provided by a plugin |
| `storage` | `.vibe/` (or the project directory) is not writable |

The exit code is 1 when any check fails, 0 otherwise; `--json` prints
`{"ok": …, "checks": [{"level", "name", "message"}]}`. Agent files are not checked here;
`vibe agents list` reports an invalid one. Run `vibe doctor` after `vibe init` and whenever
a run fails before its first phase.

## `vibe completions`

```sh
vibe completions <SHELL>
```

Prints a completion script for `bash`, `zsh`, `fish`, `elvish` or `powershell`. Installation
is shown in [Installation](installation.md#shell-completions).

## JSON output

`--json` replaces human-oriented output with one JSON document on standard output, suitable
for scripts; logs still go to standard error. Field names follow the persisted types
([Domain model](../design/domain-model.md)) and statuses are the `snake_case` names listed
above. `task add` prints `number`, `id`, `dir` and `task`; `task list` an array of
`{number, task}`; `task show` the `number`, `task`, `dir`, `spec`, `plan`, `qa_reports`,
`run` and `worktree` (branch, path and whether each exists); `init` the paths it wrote.
`vibe run --json` is the exception: a stream of events, one per line, then the summary.
