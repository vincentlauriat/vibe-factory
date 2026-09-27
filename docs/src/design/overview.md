# Architecture overview

Vibe Factory is a Cargo workspace of eight crates arranged in strict layers. Every arrow
points downwards; no crate depends on a crate above it.

```text
                        ┌─────────────┐
                        │  vibe-cli   │   command line: wires everything together
                        └──────┬──────┘
        ┌──────────┬───────────┼───────────┬────────────┐
        │          │           │           │            │
        │   ┌──────▼──────┐    │           │            │
        │   │vibe-pipeline│    │           │            │   orchestration, task store, run state
        │   └──────┬──────┘    │           │            │
        │   ┌──────▼──────┐    │           │            │
        │   │ vibe-agents │    │           │            │   agentic loop, prompts, structured output
        │   └──────┬──────┘    │           │            │
 ┌──────▼─────┐    │    ┌──────▼─────┐ ┌───▼─────────┐ ┌▼─────────────┐
 │vibe-provid.│    │    │ vibe-tools │ │vibe-workspac│ │ vibe-plugins │
 └──────┬─────┘    │    └──────┬─────┘ └───┬─────────┘ └┬─────────────┘
        └──────────┴───────────┴───────────┴────────────┘
                         ┌──────▼──────┐
                         │  vibe-core  │   domain model, traits, registry, events
                         └─────────────┘
```

Only `vibe-pipeline` depends on `vibe-agents`; every other crate depends on `vibe-core`
alone and reaches the others through the traits in the `Registry`. The CLI is the only
place where concrete implementations meet.

## The three ideas

**1. The core is data and traits, nothing else.** `vibe-core` has no I/O, no HTTP, no git.
It defines what a task, a spec, a plan, a QA report and a message *are*, and what a
provider, a tool, an agent, a workspace, a memory store and a hook *do*. Everything above it
is an implementation of those traits; everything can be replaced.

**2. Agents are declarative.** An agent is an `AgentSpec`: a role, a system prompt template,
a tool selection, a model selection and a thinking level. It can be loaded from TOML, built
in code or handed over by a plugin. The runtime (`vibe-agents`) executes any spec the same
way: the agentic loop calls the model, executes the tools it asks for, feeds results back,
and stops on a well-defined set of conditions.

**3. The pipeline is a state machine over persisted artefacts.** Each phase reads artefacts
written by the previous one (`task.json`, `spec.md`, `plan.json`, `qa_report.json`, progress
notes) from the task directory and writes its own. Because state lives on disk, a run can be
inspected, resumed after a crash, or continued by a human.

## Request flow

1. The CLI loads `.vibe/config.toml`, builds the **Registry** (built-in providers, tools,
   agents and workspaces, then plugins), and opens the **task store**.
2. `Pipeline::run(task)` publishes `RunStarted`, then walks the phases chosen by the
   complexity profile.
3. For each phase the pipeline picks the model for that phase, resolves the agent spec,
   builds an `AgentRunner`, and runs it with a kickoff message containing the relevant prior
   artefacts.
4. Structured outputs (JSON) are parsed into domain types and saved; free-form outputs are
   saved as markdown.
5. QA loops with the fixer until approval or the round limit; merge integrates the worktree
   or leaves the branch for a human.
6. Every step publishes **events** on the `EventBus`; the CLI renders them live and the task
   store appends them to `events.jsonl`.

## Reading order

- [Crates](crates.md) — what each crate owns.
- [Domain model](domain-model.md) — the types.
- [The pipeline](pipeline.md) — phases, profiles, retries, parallelism.
- [Agent runtime](agent-runtime.md) — the loop and its stop conditions.
- [Extension points](extension-points.md) — the traits and how to implement them.
- [Plugin protocol](plugin-protocol.md) — the wire contract.
- [Security model](security.md) — what agents may and may not do.
- [Persistence layout](persistence.md) — what is on disk and why.
- [ADRs](adr/README.md) — the decisions and their rationale.
