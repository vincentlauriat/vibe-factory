# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
- First real-model baseline, partial: Claude Sonnet 5 passed 16/16 runs over six cases
  (`evals/baselines/`).

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

[Unreleased]: https://github.com/vincentlauriat/vibe-factory/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/vincentlauriat/vibe-factory/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/vincentlauriat/vibe-factory/releases/tag/v0.1.0
