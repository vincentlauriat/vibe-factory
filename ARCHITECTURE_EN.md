# Vibe Factory — Architecture (source of truth)

> French mirror: [ARCHITECTURE.md](ARCHITECTURE.md). The full design book lives in
> [`docs/`](docs/src/design/overview.md) (mdBook); this file is the executive summary.

## Purpose

An open, extensible Rust framework for autonomous multi-agent software development: a task
description goes through *assess → spec → plan → build → qa ⇄ fix → merge*, executed by
cooperating AI agents inside an isolated git workspace.

## Layered crates

```text
vibe-cli → { vibe-pipeline → vibe-agents, vibe-providers, vibe-tools, vibe-workspace, vibe-plugins } → vibe-core
```

Every crate depends on `vibe-core` only, except `vibe-pipeline` which also depends on
`vibe-agents`. Concrete implementations meet only in `vibe-cli`, through the `Registry`.

| Crate | Owns |
|-------|------|
| `vibe-core` | Domain types (Task, Spec, Plan, Subtask, QaReport, Message), traits (`ModelProvider`, `Tool`, `WorkspaceProvider`, `MemoryStore`, `TaskStore`, `Hook`, `Plugin`), `AgentSpec`, `Registry`, `EventBus`, `VibeConfig`, `PromptTemplate`. No I/O. |
| `vibe-providers` | Anthropic Messages API, OpenAI-compatible chat completions (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama), mock provider, retry/backoff, HTTP error classification, provider registry. |
| `vibe-tools` | Built-in tools (`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`), shell command parser and security policy, output truncation. |
| `vibe-workspace` | Git worktree provider (open/reopen, changes, merge, discard), in-place provider, optional AI-assisted conflict resolution, commit helpers. |
| `vibe-plugins` | Vibe Plugin Protocol (JSON-RPC 2.0 over stdio, MCP-shaped), plugin process client, manifests and discovery, plugin host, `PluginServer` helper for writing plugins in Rust. |
| `vibe-agents` | `AgentRunner` agentic loop (parallel non-mutating tools, hooks, context budgeting, cancellation), structured-output extraction and repair, continuation after context exhaustion, built-in prompts and agent specs, TOML agent overrides. |
| `vibe-pipeline` | File-backed `TaskStore` under `.vibe/tasks/NNN-slug/`, complexity profiles, phase orchestration, parallel subtask execution honouring `depends_on`, QA/fix loop with escalation, run state and resume, event log. |
| `vibe-cli` | `vibe` binary: `init`, `task add|list|show|discard`, `run`, `status`, `config`, `agents`, `plugins`, `doctor`; live event rendering. |

## Key design rules

1. **Core is data + traits.** Everything concrete implements a core trait and is collected in a `Registry`; the pipeline only talks to the registry.
2. **Agents are declarative** (`AgentSpec`), executed by one runtime; users override or add agents in `.vibe/agents/*.toml`.
3. **Artefacts on disk are the state machine.** Each phase reads the previous artefacts and writes its own; runs are inspectable and resumable.
4. **Structured verdicts.** Complexity, spec, plan and QA are JSON parsed into typed structs (with repair + one retry); markdown is rendered from them, never parsed.
5. **Isolation by default** via git worktrees; `in_place` for experiments; other isolation via `WorkspaceProvider` plugins.
6. **Defence in depth for the shell**: parsed segments, blocked programs, per-command validators, optional allowlist, path containment, timeouts.
7. **Plugins in any language** through the stdio protocol; MCP servers work as tool plugins.

## Persistence (`.vibe/`)

```text
.vibe/
  config.toml                 project configuration
  agents/*.toml               agent overrides
  plugins/<name>/vibe-plugin.toml
  worktrees/<slug>-<id>/      isolated workspaces
  tasks/NNN-slug/
    task.json  spec.md  spec.json  plan.json  qa_report_<n>.json  qa_report_<n>.md
    progress.md  memory/{gotchas.md,patterns.md}  events.jsonl  run.json
  tool-output/<uuid>.txt      full text of truncated tool outputs
```

## Quality gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets` with `-D warnings`,
`cargo test --workspace`, `cargo doc` with `-D warnings`, `mdbook build docs`, on Linux,
macOS and Windows (GitHub Actions). `unsafe_code` forbidden. Every public item documented.
