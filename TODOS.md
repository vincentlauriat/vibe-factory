# TODOS

Working list for the next releases. The public, shorter version is
[docs/src/reference/roadmap.md](docs/src/reference/roadmap.md); the architecture behind the
user interfaces is [ADR-007](docs/src/design/adr/007-one-seam-many-interfaces.md).

Legend: `[x]` done, `[ ]` to do, `[~]` in progress.

## 0.3 — interaction foundations and terminal UI

A graphical interface needs an engine that can stream, wait for a human, survive the
terminal that started it and describe itself with stable events. 0.3 builds that engine,
then a terminal UI on top of it.

### 1. Real-model baseline (carried over from 0.2)
- [x] Run `evals/run_suite.py` with 3 repetitions on at least one named model
      (Claude Sonnet 5, stopped after 16 runs: 16/16)
- [x] Publish `summary.md` with the framework commit, toolchain and model
      (`evals/baselines/2026-09-27-claude-sonnet-5.md`)
- [ ] Finish the suite: `lru`, `money`, `semver`, `slug` and two more `inventory` runs
- [ ] Review a sample of runs by hand (tests added, shared code in refactoring cases)
- [x] `run_suite.py --jobs N` to run cases in parallel (a full suite takes about 2 hours)

### 2. Streaming completions
- [x] `ModelProvider::stream` with a default that falls back to `complete`
- [x] Anthropic SSE streaming (text, thinking, tool use blocks, usage)
- [x] OpenAI-compatible SSE streaming (text, tool call deltas, usage)
- [x] `Event::AgentDelta { run, role, text }` published while a step streams
- [x] `vibe --json` shows deltas (not kept in events.jsonl); mock provider streams word by word

### 3. Events v2
- [x] Sequence number on every envelope, per run, persisted in `events.jsonl`
- [x] New events: `ValidationFinished`, `SubtaskIntegrated`, `BudgetUpdated`,
      `ArtefactWritten` (spec, plan, QA report); approval events come with step 4
- [x] Versioned event schema (`schema` in each envelope) documented in docs/src/reference/events.md, checked by a test
- [x] `vibe events <task> [--follow] [--after SEQ] [--all]` replays and tails a run's events

### 4. Human approvals
- [x] `pipeline.approvals = ["spec", "plan", "merge"]` (default none)
- [x] Gate after the listed phases: the run pauses with an approval request stored in
      `run.json`
- [x] `vibe approve <task> [--comment]` / `vibe reject <task> --reason` resolve it;
      a rejection feeds the reason back to the phase that produced the artefact
- [x] Resuming an approved run continues; resuming an unanswered one pauses again

### 5. Run manager and cross-process safety
- [x] Cross-process lock on `.vibe/` writes (index) and one live run per task
      (lock file with pid, stale lock detection)
- [x] `vibe-pipeline::RunManager`: start, resume, cancel, list active runs, subscribe to
      events (in-process), used by the CLI
- [x] `vibe cancel <task>` asks a running `vibe run` in another terminal to stop

### 6. Terminal UI
- [x] `vibe tui`: task board by status, run view with phases, subtasks, live agent text,
      tool calls and budget gauge
- [x] Keys: new task, run, resume, cancel, approve, reject; tabs for activity, plan and changes
- [x] Works on the event log and live deltas only; runs from other processes show up

### 0.2 debts
- [x] Count assisted merge-resolution calls in the run budget
- [x] Try an assisted resolution before retrying a subtask whose integration conflicts
- [x] Run the live container test in CI (pull a pinned image on Linux; in the pending ci.yml)

## 0.4 — server, web UI and integrations

### 7. `vibe serve`
- [x] HTTP API over the run manager: tasks, runs, artefacts, diff, approvals, cancel
- [x] Server-sent events stream with replay from a sequence number
- [x] Bound to localhost by default, bearer token, CSRF protection, no secrets in responses
- [x] API reference table in the CLI guide (a full OpenAPI file is left for later)

### 8. Web UI
- [x] Static single-page app embedded in the binary
- [x] Board, activity timeline, changes, spec, plan and QA views, approvals (plan editing left for later)
- [x] Evaluation dashboard from `summary.json`

### 9. Integrations and capabilities
- [x] Issue importers (GitHub, GitLab) as `TaskSource`; Linear left for later
- [x] Open a pull request when a task is ready
- [x] Web fetch and search tools behind the network permission and a domain allowlist
- [x] Persistent memory across tasks (`.vibe/memory.jsonl`), deduplicated, `vibe memory`
- [x] VS Code extension on the server API (editors/vscode)

## Ideas
- Semantic merge assistance for parallel tasks touching the same files
- Prompt caching statistics and optimisation hints
- Cost estimates from a user-supplied price table
- A gallery of community agents and plugins
