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

## Next

- [ ] Web fetch and web search tools (with domain allow/deny lists)
- [ ] Streaming completions and live token output in the CLI
- [ ] Terminal UI board (`vibe board`) with live events
- [ ] HTTP/WebSocket server crate so desktop or web front-ends can drive the pipeline
- [ ] Issue tracker importers (GitHub, GitLab) as plugins
- [ ] Container workspace provider (Docker/Podman) as a plugin
- [ ] Persistent memory store with full-text search, shared across tasks
- [ ] Multi-account provider pools with automatic fail-over on rate limits
- [ ] Cost and token budgets per run with hard stops
- [ ] Changelog and release-notes generation from completed tasks

## Ideas

- Semantic merge assistance for parallel tasks touching the same files
- Prompt caching statistics and optimisation hints
- A gallery of community agents and plugins

Contributions to any item are welcome — see [Contributing](contributing.md).
