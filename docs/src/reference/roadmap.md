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

Carried over: a published real-model baseline. The harness is ready
([evals/README.md](https://github.com/vincentlauriat/vibe-factory/blob/main/evals/README.md));
no score will be claimed before it has been run with repetitions on named models.

## 0.3 — interaction and capabilities

- [ ] Real-model evaluation baseline, published with the framework commit and models used
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
