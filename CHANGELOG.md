# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- Anthropic: the HTTP 400 refusing an API key that is not scoped to a workspace now says to set
  `ANTHROPIC_WORKSPACE_ID` (or to use a key created in a workspace); the providers guide
  documents it.

## [0.5.1] — 2026-09-28

macOS app release: an app icon, a fix for API keys entered after a project is opened, and
Sparkle updates. The command line is unchanged apart from a new fixture test.

### Added
- macOS app: an app icon (a factory with vibe waves rising from its chimney), drawn by
  `Scripts/make-app-icon.swift`, in place of the placeholder letter.
- macOS app: Sparkle auto-update (Vibe Factory → Check for Updates…, background checks),
  fed by `apps/macos/VibeFactory/appcast.xml`. `Scripts/release.sh` signs Sparkle's nested
  binaries, EdDSA-signs the DMG and writes the appcast.
- macOS app: unit tests for the view model logic, moved into the `VibeAPI` package
  (`FeedLogic.swift`): the `(run, seq)` check between a task's backlog and its live events,
  the run the Trace tab shows, and the incremental Activity filter, checked against a filter
  from scratch over random appends, trims, reseeds, filter and pause changes.
- `event-types.json`: `Event::TYPES` as a fixture of the Swift tests, checked for drift by a
  cargo test (`VIBE_UPDATE_FIXTURES=1` rewrites it); a Swift test fails when an event type
  has no Activity group.

### Changed
- macOS app: a stream refused with 503 (the server's limit of open streams) reconnects with
  its own, longer backoff (5 s doubling up to 60 s) instead of the network one (0.5 s up to
  10 s).

### Fixed
- macOS app: adding or removing a variable in Settings (an API key) restarts the `vibe serve`
  of the open projects, after their current run if one is going. The server only read them
  when the project opened, so a key entered afterwards failed every run with `AuthFailed`.

## [0.5.0] — 2026-09-28

Visibility release: see everything that was done, from every interface. Configurations and
runs from 0.4 keep working; see [Upgrading](docs/src/user/migration.md).

### Added
- `vibe history [REF] [--all]`: what each task delivered — runs with their state, phases,
  commits, changed files (from git, including fast-forward merges, else from the events,
  flagged approximate), validations, last QA verdict, tokens and active time.
- `[pricing."<provider>/<model>"]` in `config.toml`: prices per million tokens; the history
  shows a cost only when every model a task used has one, never an estimate.
- `vibe trace <REF> [--run ID | --all] [--tool NAME] [--subtask ID] [--full]`: every tool
  call of a run with its complete arguments, duration, exit code and output.
- Trace store: the complete output of every tool call is kept in
  `.vibe/tool-output/<task dir>/<run>/<call>.txt`, git-ignored, removed by
  `vibe task discard` (`pipeline.trace_outputs`, default on; `pipeline.trace_max_chars`,
  default 100 000 per call).
- `vibe events` without a task: the activity of every task in time order, prefixed with the
  task number, with `--since`, `--type` and `--task` filters (also `--since` and `--type`
  with a task) and `--follow` from now on; `--json` prints envelopes tagged with the task.
- Events that tell the whole story (schema stays 2, older logs still load): a `call` id,
  the subtask, exit code, output length and output path on `tool_called`/`tool_returned`;
  the model on `agent_started`; new `committed` and `merged` events; the run's usage,
  active time and start on `run_finished`.
- `vibe serve`: `GET /api/events` and `GET /api/stream` for every task (filters, event
  cursor, replay from `Last-Event-ID`), `GET /api/history[/{ref}]`,
  `GET /api/tasks/{ref}/trace` and `GET /api/tasks/{ref}/trace/{call}/output`;
  `--exit-on-stdin-eof` for clients that start the server as a child process.
- Web UI: Activity view (every task, type-group filters, task filter, pause), History view
  (table and per-task detail), and a Trace tab in the task detail (arguments, complete
  output on demand, run selector).
- `vibe tui`: Activity screen (`A`), History screen (`H`) and a Trace tab in the task
  detail (`Enter` expands a call, `o` loads its complete output).
- A native macOS app, `apps/macos/VibeFactory` (SwiftUI, macOS 14+), a client of
  `vibe serve`: opens a project and starts a server for it, or connects to a running one;
  task board, task detail, approvals, live activity, menu bar item and notifications. Built
  and tested by its own CI workflow.
- The task record keeps its `branch`; `vibe pr`, `vibe task show` and `vibe task discard`
  use it instead of recomputing it.
- Library: a read layer in `vibe-pipeline` (`events_log`, `history`, `trace`), `CallId`,
  `ToolTrace`, `ModelPrice`, and `PipelineStore::{task_dir_name, task_numbers, is_running}`;
  `vibe-pipeline` now depends on `vibe-workspace`.

### Changed
- The web UI reads one event stream for the whole project instead of one per selected task.
  The per-task routes are unchanged.
- `vibe tui` reads the selected task's log incrementally instead of re-parsing it every tick,
  and loads history and traces in the background.
- `vibe events <REF> --follow` stops at the end of the run even when `--after` (or the new
  `--type` and `--since`) hide the `run_finished` event; an `--after` beyond it used to
  follow forever.

### Fixed
- `vibe events <REF> --follow` follows a resumed run to its end; it used to stop at the
  `run_finished` left by the pause.
- A run that stops on an error it did not report itself (a storage failure, for instance)
  now also ends with a `run_finished` event.
- The run lock is released explicitly when a run ends. It was only released when its file
  closed, and a child process started meanwhile by another thread (git, a validation
  command) kept a copy of the descriptor open until its `exec`, so an immediate resume
  could be refused with "already being run by another process" naming the caller's own pid.
- `vibe serve` answers 400 instead of 500 to a task reference that matches several tasks.
- JSON output no longer panics when standard output is closed early (`| head`).

## [0.4.0] — 2026-09-27

Interaction release, covering the work planned for 0.3 and 0.4 (there was no 0.3 release):
streaming, numbered events, human approvals, a terminal UI, `vibe serve` with a web UI, and
integrations. Configurations and runs from 0.2 keep working; see
[Upgrading](docs/src/user/migration.md).

### Added
- `evals/run_suite.py --jobs N` runs up to N evaluations in parallel.
- Streaming: `ModelProvider::complete_streaming`, server-sent events for Anthropic and
  OpenAI-compatible providers, `agent_delta` events while a step is generated, and
  `providers.<name>.stream = false` to opt out.
- Events v2: every logged event carries `schema` 2 and a per-run `seq`, kept across resumes;
  new `validation_finished`, `subtask_integrated`, `budget_updated`, `artefact_written`,
  `approval_requested` and `approval_resolved` events; `vibe events <task> [--after SEQ]
  [--follow] [--all]`; the envelope and every event type documented in the book.
- Human approvals: `pipeline.approvals = ["spec", "plan", "merge"]`, `vibe approve` and
  `vibe reject --reason`; a rejection goes back to the agents that produced the work.
- `RunManager`, the single seam for interfaces (ADR-007); one process at a time per task
  (OS lock on `run.lock`), cross-process index locking, and `vibe cancel [--wait]`.
- `vibe tui`: task board and live run view with approvals, budgets, plan and changes.
- `vibe serve`: HTTP API over the run manager, server-sent event stream with replay, and an
  embedded web UI (board, live activity, plan, spec, QA, changes, approvals); loopback by
  default, Host check, random bearer token. `--evals DIR` adds an evaluation dashboard.
- `web_fetch` (public hosts only, redirects checked, optional domain allowlist) and
  `web_search` (SearXNG-compatible endpoint) tools, both behind the network permission.
- Project memory in `.vibe/memory.jsonl`: lessons of a task are recalled by later ones;
  `vibe memory list|clear`, `pipeline.project_memory`.
- `vibe task import` for GitHub and GitLab issues, and `vibe pr` to push a ready task and
  open its pull request or merge request (`[integrations.github|gitlab]`).
- A VS Code extension in `editors/vscode`, a client of `vibe serve`.
- `ANTHROPIC_WORKSPACE_ID`: sent as the `anthropic-workspace-id` header, required by API keys
  that are not scoped to a workspace.

### Fixed
- The project root search stops at the git repository and never picks the home directory,
  so a `~/.vibe` left by another tool no longer makes `vibe` treat your home as the
  project (`… is not a git repository`).
- Anthropic: current models (Claude 4.6 and later, including the default `claude-sonnet-5`)
  get adaptive thinking with an effort level instead of `budget_tokens`, and no temperature,
  both of which they reject with HTTP 400. Older models keep the budget form;
  `extra.thinking` forces either one.

### Changed
- Assisted conflict resolution counts against the run budget, and also resolves conflicts
  between subtasks before an attempt is failed.
- Evaluations: each report records the pipeline error, `summary.md` lists the errors of
  failed runs, and `run_suite.py` stops after the first run rejected for credentials, the
  model name or the configuration instead of failing every run the same way.
- First real-model baseline: Claude Sonnet 5 passed 30/30 runs over the ten cases
  (`evals/baselines/`).
- The `catalog` evaluation case asks for one equality function shared by `find` and
  `search`; its reference solution follows.

## [0.2.0] — 2026-09-27

Reliability release. Configurations and runs from 0.1 keep working; see
[Upgrading from 0.1](docs/src/user/migration.md).

### Added
- Required `pipeline.validation_commands`, executed by the pipeline itself before ready or
  merge, independently of model verdicts. Failures pause for review; results persist in
  `run.json` and every check replays on resume.
- Automatic fixes of failed required commands, with fresh QA and full revalidation, bounded
  by `pipeline.max_validation_fix_attempts` (default 2, 0 for manual only) over the whole
  run, resumes included. Policy and configuration failures still need a human.
- Validated automatic integration in a detached worktree, assisted conflict resolution
  included: failing or modified candidates and stale targets are rejected before the base
  branch moves. Workspace providers opt in through `merge_validated` / `MergeValidator`.
- One git worktree per subtask attempt (`pipeline.isolate_subtasks`, on by default with
  `git_worktree` and `container`). Finished attempts are committed and merged into the task
  branch one at a time, in plan order when they finish together; a conflict leaves the task
  branch untouched and the attempt is retried from the updated branch. New
  `SubtaskWorkspaces` trait, implemented by `GitSubtaskWorkspaces`.
- Token and duration budgets that hold across resumes: `pipeline.max_tokens`,
  `pipeline.max_duration_secs`, `vibe run --max-tokens` and `--max-duration`. `run.json`
  records the tokens and active time of every invocation; a reached limit pauses the run.
  New `RunBudget`, `BudgetLimits` and `AgentRunner::budget`.
- `container` workspace: git worktrees with every shell command (and required validation)
  in a throw-away Docker or Podman container, configured by `[workspace.container]` with an
  explicit image, network (none by default), mounts, CPU, memory and process limits. Fixed
  hardening flags, no raw flag passthrough, validated settings; `vibe doctor` checks the
  runtime. New `CommandRunner` seam used by the `bash` tool.
- Evaluation suite: ten dependency-free Rust cases with independent oracles and reference
  solutions, `check_cases.py`, `run_suite.py` and `summarize.py` (success rate, wall time,
  tokens, validation attempts). Reports record the `vibe` version and framework commit.
- Migration guide, and release and MSRV (Rust 1.88) workflows in
  `.github/pending-workflows/` to be installed by a maintainer.

### Changed
- Subtasks of a `git_worktree` task are committed one by one (`vibe: complete subtask N -
  …`), with merge commits when parallel subtasks finish out of order, and a
  `vibe: checkpoint before build` commit when the task worktree had uncommitted work.
- `vibe run` also exits with 2 when a run budget pauses the run.
- Plugin writes go through a single writer task: a cancelled or timed-out request never
  leaves a partial line, so a timeout no longer closes the plugin, and a request cancelled
  before its turn is not sent.
- The shell policy is documented as a filter, not a sandbox; use the container workspace
  for isolation.

### Fixed
- `clippy::nonminimal_bool` on recent toolchains in the pipeline driver.

## [0.1.0] — 2026-09-26

Initial public release.

### Added
- `vibe-core`: domain model (tasks, specs, plans, QA reports, messages), extension traits
  (providers, tools, workspaces, memory, task store, hooks, plugins), registry, event bus,
  configuration, prompt templates.
- `vibe-providers`: Anthropic Messages API, OpenAI-compatible chat completions, mock
  provider, retry with backoff, HTTP error classification, provider registry.
- `vibe-tools`: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`;
  shell command parser and security policy; path containment.
- `vibe-workspace`: git worktree isolation, merge with conflict reporting, optional
  AI-assisted conflict resolution, in-place mode.
- `vibe-plugins`: JSON-RPC over stdio plugin protocol (MCP-compatible tools), plugin host,
  manifests and discovery, `PluginServer` helper and an example echo plugin.
- `vibe-agents`: agentic loop with parallel non-mutating tools, hooks, context budgeting,
  cancellation, structured output extraction and repair, continuation, built-in prompts and
  agent specs, TOML agent overrides.
- `vibe-pipeline`: file-backed task store, complexity profiles, phase orchestration,
  parallel subtasks with dependencies, QA/fix loop with escalation, resume.
- `vibe-cli`: `vibe init|task|run|status|config|agents|plugins|doctor`.
- Documentation: user guide, design book with ADRs, API docs; CI on three platforms.

[Unreleased]: https://github.com/vincentlauriat/vibe-factory/compare/v0.5.1...HEAD
[0.5.1]: https://github.com/vincentlauriat/vibe-factory/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/vincentlauriat/vibe-factory/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/vincentlauriat/vibe-factory/compare/v0.2.0...v0.4.0
[0.2.0]: https://github.com/vincentlauriat/vibe-factory/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/vincentlauriat/vibe-factory/releases/tag/v0.1.0
