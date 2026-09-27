# Extension points

Everything replaceable in Vibe Factory is a small trait in `vibe-core`, and every
implementation, built-in or third-party, is collected in one `Registry`
([ADR-002](adr/002-traits-as-extension-points.md)). This page gives, for each trait, its
signature (abridged), the contract implementations must honour, a minimal example and the way
it reaches the registry. Agents are not a trait but data (`AgentSpec`); they are covered at
the end, together with the `EventBus`.

All traits are `Send + Sync` and async methods use `#[async_trait::async_trait]`.
Implementations are shared as `Arc<dyn Trait>` (`SharedProvider`, `SharedTool`,
`SharedWorkspaceProvider`, `SharedMemory`, `SharedTaskStore`, `SharedHook`), so they must be
safe to call from several tasks at once. Errors are `vibe_core::Error`; pick the `ErrorKind`
that matches the failure, because callers branch on it.

## The Registry

```rust,ignore
pub struct Registry {
    pub tools: ToolRegistry,                               // by name, insertion order
    pub providers: BTreeMap<String, SharedProvider>,
    pub agents: BTreeMap<String, AgentSpec>,               // by role name
    pub workspaces: BTreeMap<String, SharedWorkspaceProvider>,
    pub memories: BTreeMap<String, SharedMemory>,
    pub hooks: Vec<SharedHook>,                            // registration order
}
```

| Method | Behaviour |
|--------|-----------|
| `add_tool`, `add_provider(name, p)`, `add_agent`, `add_workspace`, `add_memory(name, m)` | insert, replacing an entry with the same key |
| `add_hook` | append; hooks never replace each other |
| `agent(&role)`, `provider(name)` | lookups |
| `before_phase`, `before_tool` | run hooks in order; the first `Abort(r)` wins and is returned as `Abort("<hook name>: r")` |
| `after_phase`, `after_tool` | run every hook |
| `augment_prompt(role, task)` | collect every non-`None` augmentation, in hook order |

Because later registrations replace earlier ones, registration order is precedence. Within
`PluginHost::register_all`, native plugins register before out-of-process plugins, so a
remote plugin's tool overrides a native one of the same name (a warning is logged).

## ModelProvider

```rust,ignore
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;   // name, supports_tools, supports_thinking, default_model
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    async fn health_check(&self) -> Result<()> { Ok(()) }
}
```

**Contract.**

- Accept the provider-neutral request: `system`, `messages` of `ContentBlock`s, `tools`,
  `max_tokens`, `temperature`, `thinking_budget`, `stop_sequences` and opaque `extra` fields.
  Never leak a vendor wire format out of the provider.
- An empty `request.model` means "your default model".
- Retry transient failures yourself. Return `RateLimited` (with `retry_after` when known),
  `ServerError` or `Network` only once retries are exhausted.
- Map failures precisely: `AuthFailed` for missing or rejected credentials (report a missing
  key on the first call, not at construction), `ContextTooLong` when the prompt does not fit
  (the runner turns it into `AgentStop::ContextWindow`), `InvalidRequest` for anything the
  caller must fix, including billing problems.
- Return tool calls as `ContentBlock::ToolUse` with a unique `id` and `StopReason::ToolUse`.
- Scrub secrets from error messages (`vibe_providers::scrub_secrets` does it).

**Example.**

```rust
use vibe_core::provider::ProviderInfo;
use vibe_core::{CompletionRequest, CompletionResponse, Message, ModelProvider, Result, StopReason, Usage};

/// Answers every request with the last user text, reversed.
struct Reverse;

#[async_trait::async_trait]
impl ModelProvider for Reverse {
    fn info(&self) -> ProviderInfo {
        ProviderInfo { name: "reverse".into(), supports_tools: false,
                       supports_thinking: false, default_model: "reverse-1".into() }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let last = request.messages.last().map(Message::text).unwrap_or_default();
        let message = Message::assistant(last.chars().rev().collect::<String>());
        Ok(CompletionResponse { message, stop_reason: StopReason::EndTurn,
                                usage: Usage::default(), model: "reverse-1".into() })
    }
}
```

**Registration.** `registry.add_provider("reverse", Arc::new(Reverse))`. To make a new
`kind` usable from `[providers.<name>]`, teach the provider registry a factory:
`ProviderRegistry::builder().register_kind("reverse", |_name, _cfg| Ok(Arc::new(Reverse) as SharedProvider)).build(&config)?`,
then `install_into(&mut registry)`.

## Tool

```rust,ignore
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;                       // unique, snake_case
    fn description(&self) -> &str;                // shown to the model
    fn input_schema(&self) -> serde_json::Value;  // JSON schema of the input object
    fn is_mutating(&self) -> bool { false }
    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput>;
    fn spec(&self) -> ToolSpec { /* name + description + schema */ }
}
```

**Contract.**

- Resolve every user-supplied path with `ctx.resolve_path(path)`, which normalises `.` and
  `..`, resolves symlinks of the existing prefix and returns `Denied` when the path escapes
  `ctx.workspace_root` (or the allowed `extra_read_paths`).
- Honour `ctx.permissions` (`read`, `write`, `execute`, `network`) and return
  `Error::denied(…)` otherwise.
- Return `true` from `is_mutating` if the tool writes files or runs arbitrary commands. The
  runner runs consecutive non-mutating calls concurrently, so a wrong `false` creates races.
- Prefer `ToolOutput::error("sentence the model can act on")` for expected failures. An `Err`
  or a panic is caught by the runner and reported as ``Tool `x` failed/crashed: …``, but a
  precise sentence helps the model more.
- `ToolOutput::metadata` is for observers and is never shown to the model.

**Example.**

```rust
use serde_json::{Value, json};
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

/// Counts the words of a file inside the workspace.
struct WordCount;

#[async_trait::async_trait]
impl Tool for WordCount {
    fn name(&self) -> &str { "word_count" }
    fn description(&self) -> &str { "Count the words of a text file." }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<ToolOutput> {
        if !ctx.permissions.read {
            return Err(Error::denied("word_count needs read permission"));
        }
        let Some(path) = input["path"].as_str() else {
            return Ok(ToolOutput::error("missing string argument `path`"));
        };
        let full = ctx.resolve_path(path)?;
        let text = tokio::fs::read_to_string(&full).await?;
        Ok(ToolOutput::ok(text.split_whitespace().count().to_string()))
    }
}
```

**Registration.** `registry.add_tool(Arc::new(WordCount))`. Agents see it when their
`ToolSelection` is `all` or names it.

## WorkspaceProvider

```rust,ignore
#[async_trait]
pub trait WorkspaceProvider: Send + Sync {
    fn name(&self) -> &str;                                               // e.g. "git_worktree"
    async fn open(&self, project_root: &Path, task: &Task) -> Result<Workspace>;
    async fn merge(&self, workspace: &Workspace) -> Result<MergeOutcome>;
    async fn merge_validated(&self, workspace: &Workspace,
        validator: &mut dyn MergeValidator) -> Result<MergeOutcome>; // default: error
    async fn discard(&self, workspace: &Workspace) -> Result<()>;
    async fn changes(&self, _workspace: &Workspace) -> Result<String> { Ok(String::new()) }
}
```

**Contract.**

- `open` is idempotent: calling it again for the same task returns the same workspace, so an
  interrupted run can resume.
- `Workspace::root` is the directory agents are confined to; it becomes
  `ToolContext::workspace_root`.
- `merge` returns `Merged { commit }`, `NoChanges`, or `NeedsHumanReview { files }`. It must
  not leave the project half-merged: abort and restore before reporting conflicts. Use
  `Error::workspace` for failures that are not conflicts.
- `discard` is idempotent; already-missing resources are not an error.
- `changes` is a human-readable summary for reviewers.

When mandatory validation commands are configured, the pipeline calls `merge_validated`.
Implementations must prepare the combined candidate without changing the target, call
`validator.validate(&candidate).await`, reject candidate/target changes during validation,
and publish exactly the checked result. Validation errors must leave the target unchanged.
The default implementation returns an error; it never calls the unchecked `merge` method.
The in-place implementation validates the existing workspace and has no rollback boundary.

**Example.** A provider handing every task a plain scratch directory, merged by hand:

```rust
use std::path::Path;
use vibe_core::{MergeOutcome, Result, Task, Workspace, WorkspaceKind, WorkspaceProvider};

struct Scratch;

#[async_trait::async_trait]
impl WorkspaceProvider for Scratch {
    fn name(&self) -> &str { "scratch" }

    async fn open(&self, project_root: &Path, task: &Task) -> Result<Workspace> {
        let root = project_root.join(".vibe").join("scratch").join(task.slug());
        tokio::fs::create_dir_all(&root).await?;
        Ok(Workspace { root, project_root: project_root.to_path_buf(),
                       kind: WorkspaceKind::Custom("scratch".into()), branch: None, base_branch: None })
    }

    async fn merge(&self, _ws: &Workspace) -> Result<MergeOutcome> {
        Ok(MergeOutcome::NeedsHumanReview { files: vec![] })
    }

    async fn discard(&self, ws: &Workspace) -> Result<()> {
        let _ = tokio::fs::remove_dir_all(&ws.root).await; // already gone is fine
        Ok(())
    }
}
```

**Registration.** `registry.add_workspace(Arc::new(Scratch))` (keyed by `name()`), selected
with `pipeline.workspace = "scratch"`. The built-in providers come from
`vibe_workspace::provider_by_name`.

## MemoryStore

```rust,ignore
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn remember(&self, entry: MemoryEntry) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn all(&self) -> Result<Vec<MemoryEntry>>;
}
```

**Contract.** `recall` returns at most `limit` entries, most relevant first, with an
implementation-defined ranking; `all` returns every entry newest first. A `MemoryEntry` has a
`kind` (`gotcha`, `pattern`, `decision`, `discovery`, `dead_end`, or `other`), `content`,
`files`, `tags`, optional `task_id` and `recorded_at`. Use `Error::storage` for back-end
failures. The reference implementation is `InMemoryStore` (term-overlap ranking).

**Example.** A store that keeps entries in memory and matches by substring:

```rust
use vibe_core::{MemoryEntry, MemoryStore, Result};

struct Recent(tokio::sync::Mutex<Vec<MemoryEntry>>);

#[async_trait::async_trait]
impl MemoryStore for Recent {
    async fn remember(&self, entry: MemoryEntry) -> Result<()> { self.0.lock().await.push(entry); Ok(()) }
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let v = self.0.lock().await;
        Ok(v.iter().rev().filter(|e| e.content.contains(query)).take(limit).cloned().collect())
    }
    async fn all(&self) -> Result<Vec<MemoryEntry>> { Ok(self.0.lock().await.iter().rev().cloned().collect()) }
}
```

**Registration.** `registry.add_memory("recent", Arc::new(Recent(Default::default())))`.

## TaskStore

```rust,ignore
#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn save_task(&self, task: &Task) -> Result<()>;
    async fn load_task(&self, id: TaskId) -> Result<Task>;
    async fn list_tasks(&self) -> Result<Vec<Task>>;              // newest first
    async fn delete_task(&self, id: TaskId) -> Result<()>;        // with every artefact
    async fn save_spec(&self, spec: &Spec) -> Result<()>;
    async fn load_spec(&self, id: TaskId) -> Result<Option<Spec>>;
    async fn save_plan(&self, plan: &Plan) -> Result<()>;
    async fn load_plan(&self, id: TaskId) -> Result<Option<Plan>>;
    async fn save_qa_report(&self, report: &QaReport) -> Result<()>;   // one per round
    async fn load_qa_reports(&self, id: TaskId) -> Result<Vec<QaReport>>; // oldest first
    async fn append_progress(&self, id: TaskId, note: &str) -> Result<()>;
    async fn load_progress(&self, id: TaskId) -> Result<String>;
}
```

**Contract.** Saving is an upsert. A missing optional artefact is `Ok(None)`, not an error;
a missing task is `Error::storage`. Writes must be durable when the call returns, because
resuming a run relies on what is on disk. The default file-backed implementation belongs to
`vibe-pipeline` (see [Persistence layout](persistence.md)). The trait is not stored in the
`Registry`; the host hands a `SharedTaskStore` to the pipeline directly.

## Hook

```rust,ignore
#[async_trait]
pub trait Hook: Send + Sync {
    fn name(&self) -> &str;
    async fn before_phase(&self, _phase: Phase, _task: &Task) -> HookDecision { Continue }
    async fn after_phase(&self, _phase: Phase, _task: &Task, _success: bool) {}
    async fn before_tool(&self, _ctx: &ToolContext, _tool: &str, _input: &Value) -> HookDecision { Continue }
    async fn after_tool(&self, _ctx: &ToolContext, _tool: &str, _output: &ToolOutput) {}
    async fn augment_prompt(&self, _role: &AgentRole, _task: &Task) -> Option<String> { None }
}
```

**Contract.** Every method has a no-op default; override what you need. Hooks sit on the hot
path of every tool call, so keep them fast. An `Abort(reason)` from `before_tool` becomes an
error result the model reads, so write the reason as a sentence. `augment_prompt` output is
appended to the rendered system prompt, separated by a blank line.

**Example.**

```rust
use serde_json::Value;
use vibe_core::{AgentRole, Hook, HookDecision, Task, ToolContext};

struct NoPublish;

#[async_trait::async_trait]
impl Hook for NoPublish {
    fn name(&self) -> &str { "no-publish" }

    async fn before_tool(&self, _ctx: &ToolContext, tool: &str, input: &Value) -> HookDecision {
        let cmd = input["command"].as_str().unwrap_or_default();
        if tool == "bash" && cmd.contains("npm publish") {
            HookDecision::Abort("publishing packages is reserved for humans".into())
        } else {
            HookDecision::Continue
        }
    }

    async fn augment_prompt(&self, role: &AgentRole, _task: &Task) -> Option<String> {
        (*role == AgentRole::Coder).then(|| "Never bump package versions.".to_string())
    }
}
```

**Registration.** `registry.add_hook(Arc::new(NoPublish))`. Out-of-process plugins can only
provide `before_tool` (see [Plugin protocol](plugin-protocol.md#hooksbefore_tool)).

## Plugin

```rust,ignore
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str { "0.0.0" }
    fn register(&self, registry: &mut Registry) -> Result<()>;
}
```

**Contract.** `register` is synchronous and may be called more than once (the host probes a
native plugin once into a scratch registry to list its contributions), so it must only add
entries, never perform side effects. An in-process plugin may contribute anything the
`Registry` holds: tools, providers, agents, workspaces, memories and hooks.

```rust,ignore
struct TeamRules;

impl Plugin for TeamRules {
    fn name(&self) -> &str { "team-rules" }
    fn register(&self, registry: &mut Registry) -> Result<()> {
        registry.add_tool(Arc::new(WordCount)).add_hook(Arc::new(NoPublish));
        Ok(())
    }
}

let mut host = PluginHost::load(&configs).await?;   // out-of-process plugins
host.add_native(Box::new(TeamRules));
host.register_all(&mut registry)?;
```

## AgentSpec

Agents are added as data: `registry.add_agent(spec)` keyed by `spec.role.name()`. A spec
for a built-in role replaces the built-in agent; any other role adds a new one. Specs can come
from code (`AgentSpec::new`), from TOML files in `.vibe/agents/`
(`vibe_agents::apply_agent_overrides`) or from a plugin's `agents/list`. See
[Agent runtime](agent-runtime.md) and [Customising agents](../user/agents.md).

## EventBus and EventSink

```rust,ignore
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn on_event(&self, envelope: &Envelope);   // must not block for long
}
```

`EventBus::new(capacity)` (default 1 024) wraps a `tokio::sync::broadcast` channel plus a list
of sinks. `publish(event)` stamps an `Envelope`, sends it to every live `subscribe()`
receiver (a lagging receiver loses old events, as with any broadcast channel) and then
awaits every sink in order. Sinks are therefore the right place for things that must see
every event, such as the `events.jsonl` writer; subscribers suit live UIs.

```rust
use std::sync::Arc;
use vibe_core::{Envelope, EventBus, EventSink};

struct Stderr;

#[async_trait::async_trait]
impl EventSink for Stderr {
    async fn on_event(&self, e: &Envelope) {
        eprintln!("{} {:?}", e.at, e.event);
    }
}

async fn wire(bus: &EventBus) { bus.add_sink(Arc::new(Stderr)).await; }
```
