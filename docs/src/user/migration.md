# Upgrading

## From 0.4 to 0.5

Version 0.5 is about seeing everything that was done: the history of finished work, the
activity of every task in one feed, the complete trace of each tool call, and a native macOS
app on `vibe serve`. Existing `.vibe/config.toml` files keep working, and runs started with
0.4 can be resumed with 0.5.

### Upgrade

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory --tag v0.5.0 vibe-cli --force
vibe --version    # vibe 0.5.0
vibe doctor
```

Nothing on disk needs converting and the event schema stays `2`: 0.5 only adds optional
fields and two event types (`committed`, `merged`), which older clients must ignore anyway.
Logs written by 0.4 still load, with what they lack shown as such: `vibe trace` pairs their
tool calls with their results by order (`paired: by order`) and has only the 200-character
previews of their outputs, and `vibe history` marks their token and time totals as lower
bounds (`+`) unless `run.json` still describes the run, and shows no cost for them (their
model is not recorded). See [Logs recorded before 0.5](../reference/events.md#logs-recorded-before-05).

`task.json` gains a `branch` field, recorded the next time a run opens the task's workspace.
Until then `vibe pr`, `vibe task show` and `vibe task discard` compute the branch from the
title as before.

### Behaviour that changes by default

**Complete tool outputs are kept.** Every tool call's output is written to
`.vibe/tool-output/<task dir>/<run>/<call>.txt`, at most 100 000 characters per call, so the
directory grows with every run. It is ignored by git (`vibe init` has always put
`tool-output/` in `.vibe/.gitignore`; check that yours has it), removed per task by
`vibe task discard`, and can be limited or turned off:

```toml
[pipeline]
trace_max_chars = 20_000    # keep less per call
trace_outputs = false       # or keep nothing but the previews in the log
```

Outputs may contain secrets printed by tools; they never leave your machine. See
[Tool call trace](trace.md#disk-usage) for disk usage and privacy.

**`vibe events <REF> --follow` follows a resumed run to its end.** It used to stop at the
`run_finished` left by an earlier pause of the same run. It also stops at the end of the run
when `--after` (or the new `--type` and `--since`) hides the `run_finished` event, where an
`--after` beyond it used to follow forever.

**The web UI reads one event stream.** The page opens a single `GET /api/stream` for the
whole project (after loading the recent events with `GET /api/events`) instead of one
stream per selected task. The per-task routes (`/api/tasks/{ref}/events` and
`/api/tasks/{ref}/stream`) are unchanged, so the VS Code extension and other clients keep
working. A task reference that matches several tasks is now answered with 400 instead of
500 on every route.

**`vibe tui`** has three screens (`T` Tasks, `A` Activity, `H` History) and a **Trace** tab.
`Esc` still quits from the Tasks board; on the other screens it closes what is open and
goes back. In the Trace tab, `↑` `↓` select calls instead of tasks.

### New settings

| Key | What it does | Page |
|-----|--------------|------|
| `pipeline.trace_outputs` | keep the complete output of every tool call (default `true`) | [Tool call trace](trace.md) |
| `pipeline.trace_max_chars` | characters kept per call (default `100000`) | same |
| `[pricing."<provider>/<model>"]` | prices per million tokens, to show a cost in the history (none by default) | [`[pricing]`](configuration.md#pricing) |

New commands and options: `vibe history [REF] [--all]`, `vibe trace <REF>`, `vibe events`
without a task (with `--since`, `--type`, `--task`, `--follow`), `--since` and `--type` for
`vibe events <REF>`, and `vibe serve --exit-on-stdin-eof`. New server routes:
`GET /api/events`, `GET /api/stream`, `GET /api/history[/{ref}]`,
`GET /api/tasks/{ref}/trace` and `GET /api/tasks/{ref}/trace/{call}/output`; see
[`vibe serve`](cli.md#vibe-serve).

### The macOS app

`apps/macos/VibeFactory` is a SwiftUI client of `vibe serve` for macOS 14 and later. It
starts `vibe serve --exit-on-stdin-eof` for the project it opens, so it needs the 0.5
`vibe` on the machine. It is built from source for now (its README explains how); a signed
DMG comes with its first release.

### For library users

The Rust API is pre-1.0 and 0.5 changes it in a few places. Code that only uses the `vibe`
command line is not affected.

* `vibe_core::Event` has two new variants, `Committed` and `Merged`, and new fields:
  `AgentStarted::model`, `ToolCalled::{call, subtask}`,
  `ToolReturned::{call, subtask, exit_code, timed_out, output_chars, output_file}` and
  `RunFinished::{usage, active_ms, started_at}`. A `match` needs the new cases (or a
  wildcard) and `..` in struct patterns; code that builds these events must fill the fields.
  `vibe_core::CallId` identifies a tool call.
* `Task` has a new `branch` field, `PipelineConfig` new `trace_outputs` and
  `trace_max_chars` fields, and `VibeConfig` a `pricing` table of `ModelPrice`: use
  `..Default::default()` rather than full struct literals.
* `vibe_agents::ToolTrace` and `AgentRunner::tool_trace` write the complete output of each
  call; `RunContext` has a new `tool_trace` field and `RunContext::commit` takes the
  subtask the commit belongs to (`None` for checkpoints and fixes).
* `PipelineStore` has three new methods with defaults, `task_dir_name`, `task_numbers` and
  `is_running`; implement them in a custom store to name trace directories after the task
  and to report running tasks.
* `vibe_pipeline` has a read layer ([ADR-008](../design/adr/008-trace-store-and-read-layer.md)):
  the modules `events_log` (`EventReader`, `TaggedEnvelope`, `EventCursor`,
  `AllEventsFollower`), `history` (`TaskHistory`, `task_history`, `project_history`) and
  `trace` (`RunTrace`, `Call`, `run_trace`, `read_output`).
* `vibe-pipeline` now depends on `vibe-workspace` (read-only git queries of the history).

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
