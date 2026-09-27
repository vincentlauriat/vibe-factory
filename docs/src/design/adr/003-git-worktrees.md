# ADR-003: Git worktrees for isolation

**Status:** Accepted — 2026-09-26

## Context

Agents edit files and run commands. If they work in the user's checkout, a bad run leaves a
broken tree, and two tasks cannot run at once. Containers give stronger isolation but
require a daemon, images, and volume juggling that many developers do not want.

## Decision

The default `WorkspaceProvider` creates a git worktree per task under
`.vibe/worktrees/<slug>-<id>` on a branch `vibe/<slug>-<id>` from the configured base
branch. Agents are confined to that directory by path containment. Integration tries a
fast-forward, then a regular merge, and reports conflicts for a human (optionally after an
AI-assisted resolution pass) instead of guessing.

`in_place` mode exists for experiments and for repositories that are not git.

Isolation is a trait, so containers or remote sandboxes can be added as plugins without
touching the pipeline.

## Consequences

- Several tasks can run in parallel on the same repository.
- The main branch is never touched until the user (or `auto_merge`) says so.
- Worktrees consume disk space; `vibe task discard` removes them.
- Non-git projects lose isolation unless a plugin provides it.
