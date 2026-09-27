# Agent runtime

An agent is data (`AgentSpec`); `vibe-agents` executes it. One runtime, `AgentRunner`, runs
every agent the same way: it renders the system prompt, offers the selected tools, calls the
model, executes the tools the model asks for, feeds the results back, and stops on a fixed
set of conditions. This page describes the spec, the loop, and the helpers built on top of
it (structured output and continuation).

## AgentSpec

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `role` | `AgentRole` | required | built-in role or `Custom(String)`; serialised as a `snake_case` string |
| `description` | `String` | `""` | human-readable purpose |
| `system_prompt` | `String` | required | a `PromptTemplate` with `{{variable}}` placeholders |
| `tools` | `ToolSelection` | `all` | which registered tools are offered |
| `model` | `ModelSelection` | `phase` | model to use |
| `thinking` | `ThinkingLevel` | `medium` | reasoning budget |
| `max_steps` | `u32` | `200` | maximum model round trips |
| `structured_output` | `bool` | `false` | the agent must end with a JSON document |
| `max_tokens` | `u32` | `8192` | output token budget per step |

Builders: `AgentSpec::new(role, prompt)`, `with_tools`, `with_thinking`, `with_max_steps`,
`with_structured_output`, `with_description`.

### ToolSelection

`ToolSelection::resolve(available)` turns the selection into concrete names, always
intersected with what the runner's `ToolRegistry` actually contains and in registry order:

| Variant | TOML / JSON | Resolves to |
|---------|-------------|-------------|
| `None` | `"none"` | nothing |
| `ReadOnly` | `"read_only"` | available names in `READ_ONLY_TOOLS` (`read_file`, `glob`, `grep`, `list_dir`, `web_fetch`, `web_search`) |
| `ReadWrite` | `"read_write"` | `ReadOnly` plus `write_file` |
| `All` | `"all"` | every available tool, including plugin tools |
| `Named(names)` | `{"named": [...]}` | the listed names that exist; unknown names are dropped silently |

### ThinkingLevel

| Level | `budget()` | Anthropic | OpenAI-compatible `reasoning_effort` |
|-------|-----------|-----------|--------------------------------------|
| `off` | `None` | thinking disabled | not sent |
| `low` | 1 024 | `budget_tokens: 1024` | `low` |
| `medium` | 4 096 | `budget_tokens: 4096` | `medium` |
| `high` | 16 384 | `budget_tokens: 16384` | `high` |
| `max` | 32 768 | `budget_tokens: 32768` | `high` |

When a level has a budget, the runner raises the step's output budget to at least
`budget + 4096` (`effective_max_tokens`), because providers with extended thinking require
the output budget to exceed the thinking budget.

### ModelSelection

`phase` (the default) uses the model the caller configured for the current pipeline phase;
`{"fixed": {"provider": "anthropic", "model": "claude-opus-5"}}` pins one. The runner itself
receives a concrete provider and model id; honouring `ModelSelection` is the caller's
responsibility when it builds the runner.

## Building a runner

```rust,ignore
use std::sync::Arc;
use vibe_agents::{AgentRunner, builtin_agent};
use vibe_core::{AgentRole, EventBus, Permissions, Registry, ToolContext, ToolRegistry};

let runner = AgentRunner::new(provider, "claude-sonnet-5".into(), tools, Arc::new(registry), EventBus::default())
    .context_window(200_000)
    .task(task.clone())
    .tool_context(ToolContext::new(&workspace.root))
    .permissions(Permissions::local())
    .var("spec", spec.to_markdown())
    .cancel_token(cancel_rx);

let spec = builtin_agent(&AgentRole::Coder).expect("built-in");
let outcome = runner.run(&spec, "Implement subtask 2.".into()).await?;
```

| Builder | Default | Effect |
|---------|---------|--------|
| `context_window(tokens)` | `DEFAULT_CONTEXT_WINDOW` = 200 000 | basis of the 85 % / 90 % thresholds |
| `run_id(id)` | fresh `RunId` | attached to every event |
| `task(task)` | none | provides `task_title`, `task_description`, `task_id` for tools and hooks |
| `subtask(id)` | none | reported in `agent_started` |
| `tool_context(ctx)` | current directory | workspace root and permissions of tool calls |
| `permissions(p)` | `Permissions::default()` (local, no network) | applied to the tool context |
| `max_tool_output_chars(n)` | `DEFAULT_MAX_TOOL_OUTPUT_CHARS` = 100 000 | truncation threshold |
| `var` / `extra_vars` | none | prompt variables; override the built-in ones |
| `cancel_token(rx)` | none | `tokio::sync::watch::Receiver<bool>` |
| `max_provider_retries(n)` | 2 | runner-level retries of retryable provider errors |
| `retry_base_delay(d)` | 1 s | base of the runner's exponential backoff |

A runner is cheap to clone; every `run` starts a fresh conversation, and `resume(spec,
history, message)` continues an existing transcript.

## The loop

Before the first step the runner renders the system prompt (template plus the
`augment_prompt` output of every hook, each separated by a blank line), resolves the tool
selection, estimates the size of the tool schemas, sets `ctx.agent` to the role name and
`ctx.task_id` from the task, and publishes `agent_started`.

```text
loop:
  cancelled?                                   ─ yes ─▶ stop Cancelled
  steps >= max_steps?                          ─ yes ─▶ stop MaxSteps
  converging agent, steps >= ceil(75% max)?    ─ once ─▶ append CONVERGE_MESSAGE
  estimate >= 90% of context window?           ─ yes ─▶ stop ContextWindow
  estimate >= 85% of context window?           ─ once ─▶ append CONTEXT_WARNING_MESSAGE

  complete (with runner retries)
      ContextTooLong ─▶ stop ContextWindow
      Cancelled      ─▶ stop Cancelled
      other error    ─▶ stop Error{kind, message}
  steps += 1, usage += response.usage
  publish agent_text (if the answer has text)

  tool calls?  yes ─▶ execute, append assistant + one user message of results, loop
               no  ─▶ stop_reason MaxTokens and not yet nudged ─▶ append CONTINUE_NUDGE, loop
                      otherwise                                ─▶ stop Completed
```

The estimate is `Message::estimate_tokens` over the system prompt and transcript plus the
tool schemas (4 characters per token). Injected messages are merged into the last user
message when there is one, because providers reject two consecutive user turns.

| Constant | Value |
|----------|-------|
| `CONTEXT_WARNING_PERCENT` | 85 |
| `CONTEXT_STOP_PERCENT` | 90 |
| `CONVERGE_STEP_PERCENT` | 75 |
| `CONTEXT_WARNING_MESSAGE` | "You are near the context limit: finish now, write your final output." |
| `CONVERGE_MESSAGE` | "Converge now: produce your final structured answer." |
| `CONTINUE_NUDGE` | "continue (your previous answer was cut off by the output limit; resume exactly where it stopped)" |

A *converging* agent is any agent with `structured_output = true`, plus `qa_reviewer` and
`spec_critic`. Coders are never told to converge.

### Stop conditions

| `AgentStop` (tag `reason`) | Cause | `is_success()` |
|----------------------------|-------|----------------|
| `completed` | the model ended a turn without tool calls | yes |
| `max_steps` | `max_steps` round trips done | no |
| `context_window` | 90 % threshold reached, or the provider answered `ContextTooLong` | no |
| `cancelled` | cancel token set, or cancelled during a retry wait | no |
| `error` (`kind`, `message`) | non-retryable provider error or retries exhausted | no |

`run` returns `Err` only for failures outside the conversation; a provider failure is an
`AgentStop::Error` inside `Ok(AgentOutcome)`, so the transcript is never lost.
`AgentOutcome` holds `role`, `stop`, `final_text` (text of the last assistant message),
`messages`, `steps`, `tool_calls` and `usage`.

### Tool execution

The calls of one step are split into runs. Consecutive calls to non-mutating tools
(`Tool::is_mutating() == false`) run concurrently with `join_all`; a call to a mutating tool
runs alone, after everything before it and before everything after it. Results are always
returned in the order of the calls, as one `user` message of `tool_result` blocks.

For each call:

1. publish `tool_called`;
2. unknown tool: error result ``Unknown tool `x`. Available tools: …``;
3. otherwise run every hook's `before_tool` in registration order; the first `Abort` wins
   and yields an error result telling the model not to repeat the call;
4. otherwise call the tool, catching both `Err` (``Tool `x` failed: …``) and panics
   (``Tool `x` crashed: …``), then run every hook's `after_tool`;
5. truncate the output if needed and publish `tool_returned` with a 200-character preview.

Tool failures never stop the run: the model sees them and can adapt.

### Truncation and `.vibe/tool-output/`

An output longer than `max_tool_output_chars` characters is cut to that length. The full
text is written to `<workspace_root>/.vibe/tool-output/<uuid>.txt` and a note is appended:
``[Output truncated: showing the first N of M characters. The full output was saved to
`…`; read it in smaller pieces if you need more.]``. If the file cannot be written, the note
says so instead.

### Provider errors and retries

Providers retry transient failures themselves (see [Providers and
models](../user/providers.md)). The runner adds a last line of defence: a retryable error
(`RateLimited`, `ServerError`, `Network`) is retried up to `max_provider_retries` times,
waiting the provider's `retry_after` when present, else `retry_base_delay * 2^(attempt-1)`.
Each retry publishes `retrying`. The cancel token is checked after each wait.

### Events

| Event | When |
|-------|------|
| `agent_started` | before the first step |
| `agent_text` | after each step whose answer contains text |
| `tool_called` / `tool_returned` | around each tool call |
| `retrying` | before each runner-level retry |
| `agent_finished` | at the end, with steps, usage and the serialised stop |

### Cancellation

Cancellation is cooperative: the watched value is checked before every step and after each
retry delay. A tool call already in flight completes; the run then stops with `cancelled`
before the next model call.

## Structured output

Agents with `structured_output = true` end their answer with a JSON document.
`run_structured::<T>(runner, spec, message)` runs the agent and deserialises it into any
`serde` type, escalating from cheap to expensive:

1. `extract_json(final_text)`: the whole trimmed text; else the last fenced block tagged
   `json`; else the last untagged fence; else balanced top-level `{…}` / `[…]` spans from last
   to first (brackets inside strings are skipped). Only objects and arrays are accepted.
2. `repair_json`: one completion without tools, temperature 0, at most 8 192 output tokens,
   asking the model to return only valid JSON for a schema hint (the type name of `T`, or the
   hint given to `run_structured_with_hint`).
3. If the run was `cancelled` or ended in `error`, stop here with that error.
4. Resume the agent once with `Your previous answer was not valid JSON: <error>. Return only
   JSON.`, then parse again. Steps, tool calls and usage of both runs are summed.

A second failure returns `ErrorKind::InvalidRequest`. What the caller does with it (for QA,
count the round as `inconclusive`) is the caller's policy.

## Continuation

`run_with_continuation(runner, spec, message, policy)` handles long sessions. While a run
stops with `context_window` and fewer than `policy.max_continuations` continuations happened
(default 5), it summarises the transcript and starts a fresh run whose first message is the
original message followed by `Summary of your previous session:` and `Continue where you
left off.`

`summarize_transcript` renders the transcript as plain text (tool inputs cut to 400
characters, results to 1 500, the whole to the most recent 300 000 characters) and asks for
handover notes of at most `summary_max_words` (default 600) words with the sections Goal,
Done so far, Key findings, Problems, Next steps. If summarisation fails, the last outcome is
returned as is. The combined outcome appends transcripts and sums counters.

## Built-in agents

| Role | Tools | Thinking | Structured | `max_steps` | `max_tokens` |
|------|-------|----------|------------|-------------|--------------|
| `complexity_assessor` | read_only | low | yes | 30 | 8 192 |
| `spec_gatherer` | read_only | medium | yes | 100 | 16 384 |
| `spec_researcher` | read_only | medium | no | 100 | 16 384 |
| `spec_writer` | read_write | high | yes | 100 | 32 000 |
| `spec_critic` | read_only | high | yes | 60 | 32 000 |
| `planner` | read_only | high | yes | 100 | 32 000 |
| `coder` | all | low | no | 300 | 16 384 |
| `coder_recovery` | all | medium | no | 300 | 16 384 |
| `qa_reviewer` | all | high | yes | 200 | 32 000 |
| `qa_fixer` | all | medium | no | 200 | 16 384 |
| `merge_resolver` | none | low | no | 3 | 32 000 |
| `commit_message` | none | low | no | 2 | 4 096 |

Every built-in agent uses `model = phase`. Prompts live in
`crates/vibe-agents/src/prompts/<role>.md` and are embedded with `include_str!`.

## Prompt variables

Each prompt starts with an HTML comment listing its variables; `strip_doc_comment` removes it
before rendering and `documented_variables` parses it. The runner always provides the
`COMMON_VARIABLES` `task_title`, `task_description`, `workspace_root` and `date`
(`YYYY-MM-DD`); the rest come from `AgentRunner::var`. Unknown placeholders render as empty
strings.

| Variable | Used by |
|----------|---------|
| `spec` | spec_critic, planner, coder, coder_recovery, qa_reviewer, qa_fixer, merge_resolver |
| `plan` | coder, coder_recovery, qa_reviewer |
| `subtask` | coder, coder_recovery, commit_message |
| `progress` | coder, coder_recovery, qa_reviewer |
| `memory` | spec_gatherer, spec_researcher, spec_writer, planner, coder, coder_recovery, qa_reviewer, qa_fixer |
| `prior_context` | every role except commit_message |
| `qa_report` | qa_fixer |
| `spec_path` | spec_writer |
| `file_path`, `conflict` | merge_resolver |
| `diff` | commit_message |

`commit_message` and `merge_resolver` do not document `workspace_root`; `commit_message`
does not document `date` either.

## TOML overrides

`load_agent_overrides(dir)` reads every `*.toml` file of a directory (normally
`project_agents_dir(root)` = `.vibe/agents`) in file-name order; `apply_agent_overrides`
applies them to a `Registry`. Unknown keys are rejected (`deny_unknown_fields`). A file
naming an existing role changes only the keys it sets; any other role defines a new agent and
must provide `system_prompt` or `system_prompt_file` (relative to the TOML file), never both.

```toml
role = "qa_reviewer"
model = "anthropic/claude-opus-5"   # or "phase", or {fixed = {provider = "…", model = "…"}}
thinking = "max"
max_steps = 250
tools = ["read_file", "grep", "glob", "bash"]   # or "none" | "read_only" | "read_write" | "all"
system_prompt_file = "qa_reviewer.md"
```

`max_steps` and `max_tokens` are clamped to at least 1. The user-facing guide is
[Customising agents](../user/agents.md).
