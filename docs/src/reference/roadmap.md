# Roadmap

Vibe Factory is young. The list below is ordered by priority, not by date.

## 0.1 — foundation (released)

- [x] Layered crate architecture with documented extension points
- [x] Anthropic and OpenAI-compatible providers, mock provider
- [x] Built-in tools with shell security policy and path containment
- [x] Git worktree isolation and merge
- [x] Agent runtime with parallel tools, hooks, context budgeting, continuation
- [x] Spec → plan → build → QA/fix pipeline with file persistence and resume
- [x] Plugin protocol (JSON-RPC over stdio, MCP-compatible tools) and Rust plugin helper
- [x] `vibe` CLI
- [x] User guide, design book, ADRs, API docs

## 0.2 — reliability (released)

- [x] Required commands before ready/merge, persistent results and replay on resume
- [x] Feed deterministic validation failures into the bounded QA/fix loop
- [x] Validate the integration candidate, including assisted conflict resolution, before publication
- [x] Ten reproducible Rust evaluation cases with independent oracles, a suite runner and a
      summary of success, time and tokens
- [x] Worktree per subtask, ordered integration and conflict tests
- [x] Container workspace provider with explicit network, mounts and resource limits
- [x] Persistent token and duration budgets across resumes
- [x] Cancellation-safe plugin writer task
- [x] Cross-platform release qualification and migration guide

Carried over: a published real-model baseline, done in 0.3
([Claude Sonnet 5, 2026-09-27](https://github.com/vincentlauriat/vibe-factory/blob/main/evals/baselines/2026-09-27-claude-sonnet-5.md):
30/30 runs over the ten cases).

## 0.3 — interaction foundations and terminal UI (done, unreleased)

The engine learns to stream, to wait for a human and to outlive the terminal that started
it, then gets a terminal UI. See [ADR-007](../design/adr/007-one-seam-many-interfaces.md).

- [x] Real-model evaluation baseline, published with the framework commit and model
      (Claude Sonnet 5: 30/30 runs over the ten cases)
- [x] Streaming completions and live agent text
- [x] Events v2: sequence numbers, schema version, validation, integration, budget and
      artefact events, replay and follow
- [x] Human approvals after spec, plan or before merge
- [x] Run manager and cross-process locking of `.vibe/`
- [x] Terminal UI: task board and live run view

## 0.4 — server, web UI and integrations (done, unreleased)

- [x] `vibe serve`: HTTP API and server-sent events over the run manager
- [x] Local web UI: board, activity, plan, spec, QA, changes, approvals, evaluation dashboard
- [x] GitHub and GitLab issue import and pull request creation
- [x] Web fetch and search tools
- [x] Persistent project memory across tasks
- [x] VS Code extension on `vibe serve`

## Later

- OpenAPI description of the server API
- Editing the plan from the web UI before approving it
- Linear and Jira importers

## Ideas

- Semantic merge assistance for parallel tasks touching the same files
- Prompt caching statistics and optimisation hints
- A gallery of community agents and plugins

Contributions to any item are welcome — see [Contributing](contributing.md).
