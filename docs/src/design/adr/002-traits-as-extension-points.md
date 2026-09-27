# ADR-002: Traits as extension points

**Status:** Accepted — 2026-09-26

## Context

The framework must be "very open": teams will want other model vendors, other tools, other
isolation strategies (containers, remote sandboxes), other memory back-ends, and their own
agents. Hard-coding any of these makes the framework a product rather than a platform.

## Decision

Every replaceable concern is a small trait in `vibe-core`:

| Trait | Default implementation |
|-------|------------------------|
| `ModelProvider` | Anthropic, OpenAI-compatible, mock |
| `Tool` | read/write/edit/list/glob/grep/bash |
| `WorkspaceProvider` | git worktree, in place |
| `MemoryStore` | in-memory, file-backed |
| `TaskStore` | file-backed under `.vibe/tasks` |
| `Hook` | none (hooks are opt-in) |
| `Plugin` | native and out-of-process adapters |

Agents are not a trait but **data** (`AgentSpec`): a role, prompt, tool selection, model
selection and thinking level. One runtime executes every spec.

Implementations are collected in a `Registry` at start-up. The default pipeline only ever
talks to the registry, never to concrete types.

## Consequences

- Anything the built-in pipeline can do, a plugin can do.
- Tests use scripted providers and fake tools without network or filesystem.
- Trait objects (`Arc<dyn …>`) everywhere; a small runtime cost, irrelevant next to model
  latency.
- Trait evolution must be backwards compatible: new methods get default bodies.
