# Vibe Factory

**Vibe Factory** is an open, extensible framework for *autonomous multi-agent software
development*, written in Rust.

You describe what you want. A pipeline of cooperating AI agents assesses the request,
explores your codebase, writes a specification, plans the work, implements it in an isolated
git workspace, reviews the result against the acceptance criteria, fixes what the review
found, and hands you a branch ready to merge. Everything it did stays visible afterwards:
the history of each task, the activity of every task in one feed, and the complete trace of
every tool call, from the command line, a terminal UI, a local web UI or a macOS app.

```text
   task ──▶ assess ──▶ spec ──▶ plan ──▶ build ──▶ qa ──▶ fix ──▶ merge
                                          │          ▲       │
                                          └──────────┴───────┘
```

## Why another agent framework?

Most autonomous coding tools are *products*: a fixed set of agents, one model vendor, one
user interface. Vibe Factory is a *framework*. Everything that could reasonably differ
between teams is an extension point with a small, documented trait:

| You want to…                              | Implement or configure       |
|-------------------------------------------|------------------------------|
| use a different model vendor or a local model | `ModelProvider`          |
| give agents a new capability              | `Tool`                       |
| change how an agent thinks or what it may touch | `AgentSpec` (TOML, no code) |
| isolate work in a container instead of a worktree | `WorkspaceProvider` |
| remember lessons across runs              | `MemoryStore`                |
| audit, veto or enrich what agents do      | `Hook`                       |
| ship all of the above as a package        | `Plugin` (in-process); tools, agents and hooks also from any language over stdio |

The default pipeline is itself built only from these extension points, so anything the
framework can do, a plugin can do too.

## What is in the box

- **Four interfaces on one engine** — the `vibe` command line, a terminal UI (`vibe tui`), a
  local web UI and HTTP API (`vibe serve`), and a native macOS app built on that API; they
  all read the same events, so they always agree. A VS Code extension is a client of
  `vibe serve` too.
- **See what was done** — `vibe history` for what each task delivered (runs, commits,
  changed files, validations, QA verdict, tokens, active time, and a cost when you give
  prices), `vibe events` for the activity of every task, live or replayed, and `vibe trace`
  for every tool call of a run with its complete arguments, exit code and output.
- **Human control** — approvals of the spec, the plan or the merge, token and time budgets,
  cancellation and resume from any interface.
- **Providers** — Anthropic, any OpenAI-compatible API (OpenAI, Groq, Mistral, xAI,
  OpenRouter, Ollama), and a scriptable mock for tests and dry runs.
- **Tools** — read, write, edit, list, glob, grep and a sandboxed shell, all confined to the
  workspace and governed by a security policy.
- **Workspaces** — git worktree isolation with fast-forward, merge and conflict reporting,
  or in-place mode for quick experiments.
- **Agents** — complexity assessor, spec gatherer, researcher, writer and critic, planner,
  coder, QA reviewer and fixer, merge resolver and commit-message writer, all overridable
  from `.vibe/agents/*.toml`.
- **Plugins** — a JSON-RPC-over-stdio protocol shaped like the Model Context Protocol, so
  existing MCP servers work as tool plugins and new plugins can be written in any language.
- **Documentation** — this book: a user guide, a design book with architecture decision
  records, and generated API docs.

## Where to go next

- New user: start with [Installation](user/installation.md) and the
  [Quick start](user/quickstart.md), then see [History](user/history.md) and
  [Tool call trace](user/trace.md) to look back at what the agents did.
- Contributor or curious engineer: read the
  [Architecture overview](design/overview.md).
- Extending the framework: see [Extension points](design/extension-points.md) and
  [Plugins](user/plugins.md).

Vibe Factory is licensed under MIT or Apache-2.0, at your option.
