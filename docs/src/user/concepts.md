# Concepts

This page introduces the vocabulary used throughout Vibe Factory. The
[Glossary](../reference/glossary.md) has one-line definitions; this page explains how the
pieces fit together.

## Task

A **task** is the unit of work you hand to the framework: a title and a description, in
plain language, of what you want. Tasks live on a board with the statuses `backlog`,
`planning`, `building`, `review`, `ready`, `done`, `failed` and `cancelled`.

```sh
vibe task add "Add OAuth login with GitHub" \
  --description "Users should be able to sign in with their GitHub account…"
```

## Complexity

Before doing anything expensive the pipeline **assesses** the task: `trivial`, `simple`,
`standard` or `complex`. The class picks a *profile*: a trivial fix skips the
specification; a complex feature adds research and a self-critique of the spec.

## Specification

The **spec** captures what must be built and how success will be checked: a summary, a
list of requirements (functional, non-functional, constraints) each with acceptance
criteria, and the context gathered from your codebase (relevant files, conventions,
assumptions). It is stored both as JSON and as a readable `spec.md`.

## Plan and subtasks

The **plan** breaks the spec into ordered *phases* of **subtasks**. A subtask is small
enough for one fresh agent session: a title, a precise description, the files to touch,
the subtasks it depends on, and how to verify it. Subtasks without dependencies on each
other can run in parallel.

## Agents and roles

An **agent** is a model running in a loop with tools. Vibe Factory ships one agent per
**role**: complexity assessor, spec gatherer, researcher, writer and critic, planner, coder,
QA reviewer, QA fixer, merge resolver and commit-message writer. Each role has a system
prompt, a set of allowed tools, a model and a thinking level. All of it is configurable
from `.vibe/agents/*.toml` without writing code.

## Tools

**Tools** are what agents can do: read, write and edit files, list and search the tree,
and run shell commands. Tools are confined to the workspace and governed by a security
policy. Plugins can add tools (a browser, a database client, an issue tracker…).

## Workspace

Agents never work in your checkout by default. The **workspace** is a git worktree on a
dedicated branch, created per task. When QA approves, the branch is merged (or left for
you to review). `in_place` mode disables isolation for quick experiments.

## Pipeline and phases

The **pipeline** runs the phases `assess → spec → plan → build → qa → fix → merge`. Each
phase reads the artefacts of the previous ones and writes its own into
`.vibe/tasks/<task>/`. A **run** can be watched live, interrupted, and resumed.

## Providers and models

A **provider** is a model vendor or API (Anthropic, OpenAI, a local Ollama…). Models are
referred to as `provider/model`, for example `anthropic/claude-sonnet-5` or
`ollama/qwen2.5-coder`. Each phase can use a different model.

## Memory

**Memory** is what survives a single run: gotchas discovered while coding, conventions,
decisions. The default store is a set of files in the task directory; plugins can provide
richer back-ends.

## Plugins and hooks

A **plugin** bundles extensions: tools, agents, providers, workspaces, memory stores and
**hooks**. A hook observes or vetoes what agents do (for example "never let an agent run
`npm publish`") and can enrich prompts with project rules. In-process (Rust) plugins can
contribute every kind of extension; out-of-process plugins, written in any language
speaking the plugin protocol, contribute tools, agents and the `before_tool` hook.
