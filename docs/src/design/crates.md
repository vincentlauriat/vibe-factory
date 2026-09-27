# Crates

Vibe Factory is a cargo workspace of eight crates. This page describes what each one owns,
the public types you will meet first, what it is allowed to depend on, and how it is tested.
The [Architecture overview](overview.md) explains why the layering exists; this page is the
map.

## Dependency graph

The graph below is the one declared in the `Cargo.toml` files today. Every implementation
crate depends on `vibe-core` and on nothing else from the workspace: `vibe-agents` reaches
models, tools and hooks only through the core traits, so it does not link any concrete
provider, tool set, workspace or plugin transport. Wiring concrete implementations together
is the job of the layers above it.

```text
                 vibe-cli            (binary `vibe`)
                    │
                 vibe-pipeline       (orchestration, task store)
                    │
   ┌────────────┬───┴────────┬──────────────┬──────────────┬─────────────┐
   │            │            │              │              │             │
vibe-agents  vibe-providers vibe-tools  vibe-workspace  vibe-plugins     │
   │            │            │              │              │             │
   └────────────┴────────────┴──────┬───────┴──────────────┴─────────────┘
                                    │
                                vibe-core      (types + traits, no I/O)
```

`vibe-pipeline` and `vibe-cli` currently declare only `vibe-core` (both are placeholders);
the edges from the pipeline to the five middle crates are the intended wiring described in
`ARCHITECTURE_EN.md`.

| Crate | Workspace dependencies | Notable external dependencies |
|-------|------------------------|-------------------------------|
| `vibe-core` | none | `serde`, `serde_json`, `toml`, `tokio` (sync only), `chrono`, `uuid`, `indexmap`, `thiserror`, `async-trait` |
| `vibe-providers` | `vibe-core` | `reqwest` (rustls), `regex`, `tokio`; `wiremock` in tests |
| `vibe-tools` | `vibe-core` | `globset`, `ignore`, `regex`, `similar` |
| `vibe-workspace` | `vibe-core` | `tokio` (process); shells out to `git` |
| `vibe-plugins` | `vibe-core` | `tokio` (process, io), `futures`, `dirs`, `indexmap` |
| `vibe-agents` | `vibe-core` | `futures`, `tokio`, `chrono`, `uuid`, `toml` |
| `vibe-pipeline` | `vibe-core` | none yet |
| `vibe-cli` | `vibe-core` | none yet |

Workspace-wide rules apply to every crate: `unsafe_code = "forbid"`, `missing_docs = "warn"`
(promoted to an error in CI), `clippy::all = "warn"`, edition 2024. See
[ADR-001](adr/001-rust-workspace.md).

## vibe-core

**Responsibility.** The vocabulary of the framework: domain types, the extension traits, the
`Registry` that collects implementations, the `EventBus`, the configuration model and the
prompt template engine. The crate documentation states the rule bluntly: no I/O and no
network code. The only filesystem access is `VibeConfig::load`/`save` and the lenient
canonicalisation used by `ToolContext::resolve_path`.

**Key public types.**

| Module | Types |
|--------|-------|
| `task`, `spec`, `plan`, `qa` | `Task`, `TaskStatus`, `Complexity`, `TaskSource`, `Spec`, `Requirement`, `SpecContext`, `Plan`, `PlanPhase`, `Subtask`, `SubtaskStatus`, `QaReport`, `QaIssue`, `QaVerdict`, `Severity` |
| `message`, `provider` | `Message`, `ContentBlock`, `Role`, `CompletionRequest`, `CompletionResponse`, `StopReason`, `Usage`, `ModelRef`, `ToolSpec`, `ModelProvider` |
| `tool` | `Tool`, `ToolContext`, `ToolOutput`, `Permissions`, `ToolRegistry` |
| `agent` | `AgentSpec`, `AgentRole`, `ToolSelection`, `ModelSelection`, `ThinkingLevel`, `AgentStop`, `AgentOutcome` |
| `workspace`, `memory`, `store` | `WorkspaceProvider`, `Workspace`, `MergeOutcome`, `InPlaceWorkspace`, `MemoryStore`, `InMemoryStore`, `TaskStore` |
| `plugin`, `event` | `Plugin`, `Hook`, `HookDecision`, `Registry`, `Event`, `Envelope`, `EventBus`, `EventSink` |
| `config`, `prompt`, `error` | `VibeConfig`, `ProviderConfig`, `PipelineConfig`, `SecurityConfig`, `PluginConfig`, `PromptTemplate`, `Error`, `ErrorKind` |

**Must never depend on** any other workspace crate, an HTTP client, `git`, or a process API.
Adding such a dependency would force every plugin author to compile it.

**Tests.** Unit tests next to each module: serde round trips (`Task`, ids, `ContentBlock`
tags), `Plan::validate` on cycles and unknown dependencies, `ToolSelection::resolve`, path
containment in `ToolContext::resolve_path` (including `..` and absolute paths), the
`EventBus` reaching both subscribers and sinks, `VibeConfig` TOML round trip and per-phase
overrides.

## vibe-providers

**Responsibility.** Every `ModelProvider` shipped with the framework and the plumbing they
share: one pooled HTTP client, error classification, secret scrubbing, retry with
exponential backoff, and a registry that builds named providers from `[providers.<name>]`.

**Key public types.** `AnthropicProvider` (Messages API, extended thinking, prompt caching,
`AnthropicAuth::ApiKey` or `AnthropicAuth::Bearer`), `OpenAiCompatibleProvider` (Chat
Completions for OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama and gateways), `MockProvider`
and `MockStep`, `ProviderRegistry` and `ProviderRegistryBuilder` (with `register_kind` for
new kinds), `RetryPolicy` and `with_retry`, `classify_http_error`, `scrub_secrets`,
`expand_model_shorthand`.

**Must never depend on** `vibe-agents` or anything above it. A provider sees a
`CompletionRequest` and nothing about agents, tasks or phases.

**Tests.** Unit tests for classification rules (billing wording wins over a 429, context
length on 400/413), backoff arithmetic and jitter bounds, header parsing and scrubbing.
Integration tests in `tests/anthropic_http.rs` and `tests/openai_http.rs` run each provider
against a `wiremock` server: tool use round trip, 429 then success, retries exhausted with
`retry_after` preserved, 401 and billing errors not retried, malformed JSON, bearer mode and
custom headers, and replay of signed thinking blocks on the next tool turn.

## vibe-tools

**Responsibility** (from `ARCHITECTURE_EN.md`; the crate is still being written). The built-in
tools `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep` and `bash`, the shell
command parser and security policy, and output truncation. The policy is described in
[ADR-004](adr/004-shell-policy.md).

**Must never depend on** `vibe-agents` or above. Tools implement `vibe_core::Tool` and are
confined through `ToolContext`.

## vibe-workspace

**Responsibility.** Where a task's agents work and how their work reaches the main line.
Implements `WorkspaceProvider` with git worktrees, re-exports `InPlaceWorkspace`, and offers
commit helpers and an optional model-assisted conflict resolver.

**Key public types.** `GitWorktreeProvider` (fields `base_branch`, `worktrees_dir`,
`merge_strategy`), `MergeStrategy` (`Manual` or `Assisted { provider, model }`),
`resolve_conflicts` and `Resolution`, `commit_all`, `has_uncommitted`, the `Git` wrapper,
and `provider_by_name("git_worktree" | "in_place", base_branch)`.

**Must never depend on** `vibe-providers` concretely: the assisted merge takes any
`SharedProvider`. It must not depend on `vibe-agents` either; its merge prompt is a
constant (`MERGE_SYSTEM_PROMPT`) rather than an agent spec.

**Tests.** `tests/worktree.rs` creates real repositories in temporary directories and
exercises: creation and reuse of a worktree, recreation of a stale directory, refusal on a
non-repository, `changes` output, fast-forward with checkpoint commit, merge commit when the
base moved, conflicts reported for human review with the project left clean and on its
original branch, `NoChanges`, refusal on a dirty project, idempotent `discard`, and an
assisted merge driven by a `MockProvider`-style provider.

## vibe-plugins

**Responsibility.** The Vibe Plugin Protocol (VPP): wire types, a client that drives a plugin
process, adapters turning a connection into `Tool`, `Hook` and `AgentSpec` contributions,
manifest parsing and discovery, the `PluginHost` failure policy, and a `PluginServer` helper
for writing plugins in Rust. Ships the `vibe-echo-plugin` example binary. See
[Plugin protocol](plugin-protocol.md).

**Key public types.** `PluginHost`, `LoadedPlugin`, `RemotePlugin`, `RemoteTool`,
`RemoteHook`, `PluginProcess`, `PluginManifest`, `PluginServer`, `PluginLogger`,
`ToolCallContext`, `Capabilities`, constants `PROTOCOL_VERSION`, `MCP_PROTOCOL_VERSION`,
`DEFAULT_TIMEOUT`, `TOOL_CALL_TIMEOUT`, `SHUTDOWN_GRACE`.

**Must never depend on** `vibe-agents`, `vibe-tools` or `vibe-providers`. A plugin is a source
of extensions, not a consumer of the built-in ones.

**Tests.** Unit tests in `client.rs` use `tokio::io::duplex` to script a fake peer
(timeouts, errors, pagination, notifications). `tests/echo_process.rs` spawns the real
`vibe-echo-plugin` binary: tools, agents and hooks are contributed, manifest discovery finds
it, a broken optional plugin does not block the others, and a relative program resolves
against `cwd`.

## vibe-agents

**Responsibility.** The runtime that turns an `AgentSpec` into a conversation: the
`AgentRunner` loop, structured-output extraction and repair, continuation after context
exhaustion, the twelve built-in agents and their prompts, and TOML overrides. See
[Agent runtime](agent-runtime.md).

**Key public types.** `AgentRunner`, `run_structured`, `run_structured_with_hint`,
`extract_json`, `repair_json`, `run_with_continuation`, `ContinuationPolicy`,
`builtin_agents`, `register_builtin_agents`, `load_agent_overrides`,
`apply_agent_overrides`, `project_agents_dir`, `documented_variables`, and the
`test_support` module (`ScriptedProvider`, `EchoTool`, `FailingTool`, `ConcurrencyProbe`).

**Must never depend on** a concrete provider, tool, workspace or plugin crate. The lib
documentation calls this out: models come through `ModelProvider`, capabilities through
`Tool`, policies through the hooks of a `Registry`.

**Tests.** `tests/agent_loop.rs` drives the loop with `ScriptedProvider` and mock tools, no
network: parallel read-only calls, sequential mutating calls, `MaxSteps`, hook vetoes seen by
the model, the 85 % warning and 90 % stop, every structured-output path (fenced, unfenced,
repair, retry, give up), continuation and its limit, unknown, failing and panicking tools,
truncation to `.vibe/tool-output/`, cancellation, provider error handling and retries, the
max-tokens nudge, convergence at 75 %, prompt variables and hook augmentation, published
events, and a smoke run of every built-in agent.

## vibe-pipeline

**Responsibility** (from `ARCHITECTURE_EN.md`; not written yet). File-backed `TaskStore`
under `.vibe/tasks/NNN-slug/`, complexity profiles, phase orchestration, parallel subtask
execution honouring `depends_on`, the QA/fix loop with escalation, run state and resume, and
the event log. See [The pipeline](pipeline.md).

## vibe-cli

**Responsibility** (from `ARCHITECTURE_EN.md`; not written yet). The `vibe` binary with
`init`, `task add|list|show|discard`, `run`, `status`, `config`, `agents`, `plugins` and
`doctor`, and live rendering of events. See [Command line reference](../user/cli.md).

## Where to add code

| You want to… | Put it in |
|--------------|-----------|
| add a field to a persisted artefact | `vibe-core` (with `#[serde(default)]` so old files still load) |
| support a new model API | `vibe-providers`, or out of tree via `ProviderRegistryBuilder::register_kind` |
| change how the loop reacts to a stop reason | `vibe-agents::runtime` |
| change a built-in prompt | `crates/vibe-agents/src/prompts/<role>.md` |
| add a protocol method | `vibe-plugins::protocol`, then client, server and remote adapters |
| isolate work somewhere other than a worktree | a new `WorkspaceProvider`, in any crate |
