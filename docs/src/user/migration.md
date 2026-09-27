# Upgrading

## From 0.2 to 0.4

Version 0.4 includes the work planned for 0.3 (streaming, numbered events, human approvals,
the run manager and the terminal UI) and for 0.4 (`vibe serve`, the web UI, integrations).
There was no 0.3 release. Existing `.vibe/config.toml` files keep working, and runs started
with 0.2 can be resumed with 0.4.

### Upgrade

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory --tag v0.4.0 vibe-cli --force
vibe --version    # vibe 0.4.0
vibe doctor
```

Nothing on disk needs converting. Events written by 0.2 in `events.jsonl` have no `schema`
or `seq` field: they read as schema 1 and are not replayed by `vibe events`; a resumed run
numbers its new events from 1. `run.json` gains approval fields that load empty.

### Behaviour that changes by default

**Answers are streamed.** Anthropic and OpenAI-compatible providers now use server-sent
events, and `vibe --json` prints `agent_delta` lines while a step is generated (they are not
kept in `events.jsonl`). For a gateway that mishandles streaming, set
`stream = false` in its `[providers.<name>]` table. See [Streaming](providers.md#streaming).

**Project memory is on.** Lessons of a finished task are kept in `.vibe/memory.jsonl` and
the relevant ones are recalled by later tasks. Turn it off with
`pipeline.project_memory = false`; `vibe memory list` and `vibe memory clear` inspect and
reset it.

**One process per task.** A second `vibe run` on a task that is already running fails
instead of racing the first one; `vibe cancel <task>` stops a run from another terminal.

**Anthropic thinking.** Current Anthropic models (Claude 4.6 and later, including the
default `claude-sonnet-5`) get adaptive thinking with an effort level and no temperature,
which they require. Older models keep the token budget form; `extra.thinking` forces either.

### New settings, off by default

| Key | What it does | Page |
|-----|--------------|------|
| `pipeline.approvals` | pause for `vibe approve` / `vibe reject` after `spec`, `plan` or before `merge` | [Human approvals](configuration.md#human-approvals) |
| `security.web_allowed_domains`, `security.search_url` | limit `web_fetch` to some domains; the SearXNG-compatible endpoint that registers `web_search` | [Configuration](configuration.md#security) |
| `[integrations.github]`, `[integrations.gitlab]` | `vibe task import` and `vibe pr` | [CLI](cli.md) |

New commands: `vibe events`, `vibe approve`, `vibe reject`, `vibe cancel`, `vibe tui`,
`vibe serve`, `vibe memory`, `vibe task import` and `vibe pr`. A run paused for an approval
exits with code 2, like any run that waits for a human.

### For library users

The Rust API is pre-1.0 and 0.4 changes it in a few places. Code that only uses the `vibe`
command line is not affected.

* `vibe_core::Event` has new variants (`AgentDelta`, `ValidationFinished`,
  `SubtaskIntegrated`, `BudgetUpdated`, `ArtefactWritten`, `ApprovalRequested`,
  `ApprovalResolved`); a `match` on it needs a wildcard arm or the new cases.
* `vibe_core::Envelope` has new `schema` and `seq` fields: build one with `Envelope::now`.
* `ModelProvider::complete_streaming` has a default that calls `complete`; implement it to
  stream. `StreamDelta` and `DeltaSink` are new.
* `PipelineConfig` has new `approvals` and `project_memory` fields, the provider
  configuration a `stream` field, `SecurityConfig` `web_allowed_domains` and
  `search_url`, and `Config` an `integrations` table: use `..Default::default()` rather than
  full struct literals.
* `vibe_pipeline::RunManager` starts, resumes, cancels and lists runs and is the seam every
  interface uses ([ADR-007](../design/adr/007-one-seam-many-interfaces.md)).
  `RunState` has new approval fields.

## From 0.1 to 0.2

Version 0.2 is about reliability. Existing `.vibe/config.toml` files keep working, and runs
started with 0.1 can be resumed with 0.2. This page lists what changes when you upgrade and
what you may want to turn on.

### Upgrade

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory --tag v0.2.0 vibe-cli --force
vibe --version    # vibe 0.2.0
vibe doctor
```

Nothing on disk needs converting. `run.json` gains new fields (`validations`,
`validation_fix_attempts`, `pending_validation_fix`, `usage`, `active_ms`); files written by
0.1 load with empty or zero values, so a resumed 0.1 run starts its budgets from zero.

### Behaviour that changes by default

**One worktree per subtask attempt.** With the default `git_worktree` workspace, every
subtask attempt now runs in its own worktree and branch (`<task branch>--s<N>-a<attempt>`)
and is merged into the task branch as soon as it is done. You will see:

* a `vibe: checkpoint before build` commit when the task worktree had uncommitted work;
* one `vibe: complete subtask N - <title>` commit per subtask, and merge commits when
  parallel subtasks finished out of order, instead of grouped commits
  (`vibe: complete subtasks 2, 3 - …`);
* temporary `…--s2-a1` worktrees under `.vibe/worktrees/` and branches while a build runs,
  removed after each attempt.

A subtask whose changes conflict with work integrated while it ran is retried from the
updated task branch, which can cost one more attempt than in 0.1. To keep the 0.1 behaviour
(parallel subtasks share the task worktree), set:

```toml
[pipeline]
isolate_subtasks = false
```

`in_place` and plugin workspaces are unchanged. See
[One worktree per subtask attempt](workspaces.md#one-worktree-per-subtask-attempt).

**Exit code 2 has one more meaning.** `vibe run` also exits with 2 when a run budget is
exhausted (see below). Scripts that treat 2 as "QA needs a human" should read
`run_status` and `last_error` in the `--json` summary.

**Plugin timeouts no longer close the plugin.** A request that times out while its line
is being written used to close the plugin's input. Writes now go through a single writer
task that never leaves a partial line, so the plugin stays usable after a timeout. A
request cancelled before its turn is not sent at all.

### New settings, off by default

All of these are optional; leaving them out keeps the 0.1 behaviour.

| Key | What it does | Page |
|-----|--------------|------|
| `pipeline.validation_commands` | shell checks that must pass before `ready` or a merge, run by the pipeline itself | [Required validation commands](configuration.md#required-validation-commands) |
| `pipeline.max_validation_fix_attempts` | automatic fixes of failed checks over the whole run (default 2) | same |
| `pipeline.max_tokens` | pause a run after this many tokens, resumes included | [Run budgets](configuration.md#run-budgets) |
| `pipeline.max_duration_secs` | pause a run after this much active time, resumes included | same |
| `pipeline.workspace = "container"` | run the agents' shell commands in a container | [Container workspace](workspaces.md#container-workspace) |

`vibe run` gains `--max-tokens` and `--max-duration` to set the budgets for one invocation.

A reasonable starting point for a Rust project:

```toml
[pipeline]
validation_commands = ["cargo fmt --all -- --check", "cargo test --offline"]
max_tokens = 3_000_000
max_duration_secs = 7200
```

### For library users

The Rust API is pre-1.0 and 0.2 changes it in a few places. Code that only uses the `vibe`
command line is not affected.

* `vibe_core::PipelineConfig` has new public fields (`validation_commands`,
  `max_validation_fix_attempts`, `isolate_subtasks`, `max_tokens`, `max_duration_secs`).
  Build it with `PipelineConfig { …, ..PipelineConfig::default() }` rather than a full
  struct literal.
* `vibe_pipeline::PipelineDeps` has a new `subtask_workspaces` field. Pass `None` to share
  the task workspace between subtasks, or `Some(Arc::new(GitSubtaskWorkspaces::new()))`
  with a git worktree workspace.
* `vibe_pipeline::RunContext` has new `budget` and `subtask_workspaces` fields, and
  `RunState` new `validations`, `validation_fix_attempts`, `pending_validation_fix`,
  `usage` and `active_ms` fields.
* `WorkspaceProvider` has a new `merge_validated` method used when validation commands
  are configured with `auto_merge`. Its default fails closed: a custom provider must
  implement it to support validated automatic merges.
* New traits and types: `vibe_core::SubtaskWorkspaces` and `SubtaskIntegration`
  (implemented by `vibe_workspace::GitSubtaskWorkspaces`), `MergeValidator`, and
  `RunBudget`, `BudgetLimits`, `BudgetExceeded`. `AgentRunner::budget` attaches a budget to
  an agent run; a reached limit stops it like a cancellation.
