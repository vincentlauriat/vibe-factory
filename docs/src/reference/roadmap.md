# Roadmap

Vibe Factory is young. The list below is ordered by priority, not by date.

## 0.1 — foundation (this release)

- [x] Layered crate architecture with documented extension points
- [x] Anthropic and OpenAI-compatible providers, mock provider
- [x] Built-in tools with shell security policy and path containment
- [x] Git worktree isolation and merge
- [x] Agent runtime with parallel tools, hooks, context budgeting, continuation
- [x] Spec → plan → build → QA/fix pipeline with file persistence and resume
- [x] Plugin protocol (JSON-RPC over stdio, MCP-compatible tools) and Rust plugin helper
- [x] `vibe` CLI
- [x] User guide, design book, ADRs, API docs

## 0.2 — reliability (in development)

- [x] Required commands before ready/merge, persistent results and replay on resume
- [x] Three reproducible Rust evaluation fixtures and an independent acceptance runner
- [ ] Run real-model evaluations; expand the suite to ten tasks and record success, time and tokens
- [x] Feed deterministic validation failures into the bounded QA/fix loop
- [x] Validate the integration candidate, including assisted conflict resolution, before publication
- [ ] Worktree per subtask, ordered integration and conflict tests
- [ ] Container workspace provider with explicit network, mounts and resource limits
- [ ] Persistent token and duration budgets across resumes
- [ ] Cancellation-safe plugin writer task
- [ ] Cross-platform release qualification and migration guide

## 0.3 — interaction and capabilities

- [ ] Streaming completions and live token output
- [ ] Terminal UI board with live events
- [ ] Web fetch and search tools
- [ ] Richer persistent memory across tasks
- [ ] HTTP/WebSocket server and issue tracker importers

## Ideas

- Semantic merge assistance for parallel tasks touching the same files
- Prompt caching statistics and optimisation hints
- A gallery of community agents and plugins

Contributions to any item are welcome — see [Contributing](contributing.md).
