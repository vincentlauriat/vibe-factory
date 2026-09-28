<div align="center">

# Vibe Factory

**An open, extensible framework for autonomous multi-agent software development, in Rust.**

[![CI](https://img.shields.io/github/actions/workflow/status/vincentlauriat/vibe-factory/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/vincentlauriat/vibe-factory/actions)
[![Release](https://img.shields.io/github/v/release/vincentlauriat/vibe-factory?style=flat-square&include_prereleases)](https://github.com/vincentlauriat/vibe-factory/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](#license)
[![Docs](https://img.shields.io/badge/docs-book-informational?style=flat-square)](https://vincentlauriat.github.io/vibe-factory/)

</div>

You describe a task. A pipeline of cooperating AI agents assesses it, explores your
codebase, writes a specification, plans the work, implements it in an isolated git
worktree, reviews the result against the acceptance criteria, fixes what the review found,
and hands you a branch ready to merge. Afterwards you can see everything it did: what each
task delivered, the activity of every task, and every tool call with its complete output.

```text
   task ──▶ assess ──▶ spec ──▶ plan ──▶ build ──▶ qa ──▶ fix ──▶ merge
                                          │          ▲       │
                                          └──────────┴───────┘
```

Vibe Factory is a **framework**, not a product: every part that could differ between
teams — model vendor, tools, agents, isolation strategy, memory, policies — is a small
documented trait, and plugins can be written in Rust or in any language.

## Features

| Feature | Status |
|---------|--------|
| Multi-agent pipeline: assess → spec → plan → build → QA ⇄ fix → merge | ✅ |
| Complexity profiles (trivial / simple / standard / complex) | ✅ |
| Parallel subtasks honouring dependencies | ✅ |
| Git worktree isolation, fast-forward or merge, conflict reporting | ✅ |
| Providers: Anthropic, OpenAI-compatible (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama), mock | ✅ |
| Per-phase model and thinking level | ✅ |
| Built-in tools: read, write, edit, list, glob, grep, policy-checked shell | ✅ |
| Shell security policy (blocked programs, per-command validators, allowlist) and path containment | ✅ |
| Agents as data: override or add agents in `.vibe/agents/*.toml` | ✅ |
| Hooks: observe, veto and enrich agent actions | ✅ |
| Plugins: JSON-RPC over stdio, MCP-compatible tools, Rust helper | ✅ |
| Structured artefacts (JSON) with repair and retry | ✅ |
| Resume a run from its persisted state | ✅ |
| Required validation commands before ready/merge, with automatic fixes | ✅ |
| One git worktree per subtask attempt, integrated one at a time | ✅ |
| Token and duration budgets that hold across resumes | ✅ |
| Container workspace: shell commands isolated with explicit network, mounts and limits | ✅ |
| Reproducible evaluation suite (ten Rust tasks, independent oracles) | ✅ |
| `vibe` CLI with live event output | ✅ |
| Streaming answers, numbered events, `vibe events --follow` | ✅ |
| Human approvals of the spec, plan or merge | ✅ |
| `vibe tui`: task board and live run view | ✅ |
| User guide, design book, ADRs, API docs | ✅ |
| `vibe serve`: HTTP API, event stream and web UI, evaluation dashboard | ✅ |
| Import GitHub/GitLab issues, open pull requests | ✅ |
| `web_fetch` / `web_search` tools, project memory across tasks | ✅ |
| VS Code extension on `vibe serve` ([editors/vscode](editors/vscode)) | ✅ |
| `vibe history`: runs, commits, changed files, validations, QA verdict, tokens, active time and optional cost of each task | ✅ |
| Global activity: the events of every task in one feed (`vibe events`, web and terminal Activity views) | ✅ |
| Full trace: every tool call with its complete arguments, exit code and output (`vibe trace`, web and terminal Trace tabs) | ✅ |
| Native macOS app on `vibe serve` ([apps/macos/VibeFactory](apps/macos/VibeFactory)) | ✅ |

## Install

Requires Rust 1.88+ and git.

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory vibe-cli
```

The macOS app (macOS 14+) ships as a signed, notarized DMG, `VibeFactory-0.5.0.dmg`, on the
[v0.5.0 release](https://github.com/vincentlauriat/vibe-factory/releases/tag/v0.5.0); it updates itself with Sparkle. It needs the `vibe` binary above. To
build it yourself, see [apps/macos/VibeFactory](apps/macos/VibeFactory/README.md).

## Quick start

```sh
cd your-project
export ANTHROPIC_API_KEY=sk-ant-…          # or OPENAI_API_KEY, or run Ollama locally
vibe init                                  # writes .vibe/config.toml
vibe task add "Add a --json flag to the export command"
vibe run 1                                 # watch the agents work
vibe task show 1                           # spec, plan, QA report, branch
git merge vibe/add-a-json-flag-…           # or set pipeline.auto_merge = true
```

Try it without any API key:

```sh
vibe run 1 --provider mock --dry-run
```

Full guide: **[vincentlauriat.github.io/vibe-factory](https://vincentlauriat.github.io/vibe-factory/)**.

## See what was done

```sh
vibe history                 # what each finished task delivered: runs, commits, files, tokens, cost
vibe trace 1 --full          # every tool call of task 1's last run, with its complete output
vibe events --follow         # the activity of every task, live
```

`vibe tui` and `vibe serve` show the same history, activity and trace in a terminal UI and a
web UI. See [History](https://vincentlauriat.github.io/vibe-factory/user/history.html) and
[Tool call trace](https://vincentlauriat.github.io/vibe-factory/user/trace.html).

## Extend it

| You want to… | Implement or configure |
|--------------|------------------------|
| use another model vendor or a local model | `ModelProvider` |
| give agents a new capability | `Tool` |
| change how an agent thinks or what it may touch | `AgentSpec` in TOML |
| isolate work in a container | `WorkspaceProvider` |
| remember lessons across runs | `MemoryStore` |
| audit, veto or enrich agent actions | `Hook` |
| package any of the above | `Plugin` (Rust) or the stdio protocol (any language) |

See [Extension points](https://vincentlauriat.github.io/vibe-factory/design/extension-points.html)
and [Plugins](https://vincentlauriat.github.io/vibe-factory/user/plugins.html).

## Project layout

```text
crates/
  vibe-core/        domain model, traits, registry, events, config   (no I/O)
  vibe-providers/   Anthropic, OpenAI-compatible, mock; retries; registry
  vibe-tools/       built-in tools and shell security policy
  vibe-workspace/   git worktree isolation and merge
  vibe-plugins/     plugin protocol, host, manifests, Rust plugin helper
  vibe-agents/      agentic loop, structured output, prompts, built-in agents
  vibe-pipeline/    task store, orchestration, resume, read layer (history, trace, events)
  vibe-cli/         the `vibe` binary: commands, terminal UI, HTTP server and web UI
apps/macos/         native macOS app, a client of `vibe serve` (SwiftUI)
editors/vscode/     VS Code extension, a client of `vibe serve`
evals/              reproducible evaluation suite
docs/               mdBook: user guide, design book, ADRs
```

## Development

Behaviour is measured with [reproducible evaluations](evals/README.md). Upgrading from an earlier version:
see [the migration guide](docs/src/user/migration.md). What is done and what comes next:
[the roadmap](docs/src/reference/roadmap.md) and [the changelog](CHANGELOG.md).

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
mdbook serve docs
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [ARCHITECTURE_EN.md](ARCHITECTURE_EN.md)
(French: [ARCHITECTURE.md](ARCHITECTURE.md)).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in the work by you shall be dual
licensed as above, without any additional terms or conditions.
