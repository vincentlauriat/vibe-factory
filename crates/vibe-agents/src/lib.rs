//! # vibe-agents
//!
//! Agent runtime (the agentic tool-use loop) and the built-in agent
//! definitions of **Vibe Factory**.
//!
//! This crate depends only on `vibe-core`: models are reached through
//! [`vibe_core::ModelProvider`], capabilities through [`vibe_core::Tool`],
//! and policies through the hooks of a [`vibe_core::Registry`].
//!
//! ## The loop
//!
//! [`AgentRunner::run`] turns an [`vibe_core::AgentSpec`] into a conversation:
//!
//! ```text
//!  system prompt = render(spec.system_prompt, vars) + hook augmentations
//!  tools         = spec.tools.resolve(registered tools)
//!
//!  loop:
//!    cancelled?                        yes -> stop: Cancelled
//!    steps >= max_steps?               yes -> stop: MaxSteps
//!    converging agent at 75% steps?    once -> inject "Converge now"
//!    prompt >= 90% of context window?  yes -> stop: ContextWindow
//!    prompt >= 85% of context window?  once -> inject "finish now"
//!
//!    provider.complete(request)        error -> stop: Error
//!    publish AgentText
//!    tool calls requested?
//!      yes: for each call, in order:
//!             before_tool hooks        Abort -> error result
//!             consecutive read-only calls run concurrently,
//!             a mutating call runs alone
//!             after_tool hooks, truncate long outputs
//!           append assistant message + one user message holding every
//!           tool result (same order as the calls), then loop
//!      no:  MaxTokens                  -> append "continue", loop
//!           MaxTokens twice in a row   -> stop: Error ("output truncated twice")
//!           otherwise                  -> stop: Completed
//! ```
//!
//! Provider errors never become `Err`: the run ends with
//! [`vibe_core::AgentStop::Error`] and the transcript is kept in the
//! [`vibe_core::AgentOutcome`]. Tool failures (unknown tool, `Err`, panic,
//! hook veto) are sent back to the model as error results so it can adapt.
//! Retryable provider errors are retried after at most
//! [`runtime::MAX_RETRY_DELAY`]; the wait is interrupted by cancellation.
//!
//! When an answer is cut by the output limit and continued, the outcome's
//! `final_text` is the concatenation of every piece, from the cut message to
//! the last one, so a JSON document split across two turns still parses.
//!
//! Tool outputs longer than the configured limit (default 100 000
//! characters, see [`AgentRunner::max_tool_output_chars`]) are truncated; the
//! full text is saved to `.vibe/tool-output/UUID.txt` in the workspace and the
//! model is told where.
//!
//! ## Prompt variables
//!
//! The runner provides `task_title`, `task_description`, `workspace_root` and
//! `date`. Role-specific variables (`spec`, `plan`, `subtask`, `progress`,
//! `memory`, `qa_report`, `prior_context`, …) are supplied by the caller with
//! [`AgentRunner::var`]. Each built-in prompt documents its variables in a
//! leading HTML comment, stripped before rendering (see [`prompts`]).
//!
//! ## Structured output
//!
//! Agents with `structured_output = true` end their answer with a JSON
//! document. [`run_structured`] runs the agent and deserializes that document
//! into any `serde` type:
//!
//! 1. [`extract_json`] tries the whole text, the last `json` fence, then the
//!    last balanced `{…}` / `[…]` span;
//! 2. on failure, [`repair_json`] asks the model (no tools) to fix the JSON;
//! 3. on failure again, the agent is resumed once with "Your previous answer
//!    was not valid JSON: …".
//!
//! Converging agents (QA reviewer, spec critic, and any structured agent) are
//! told to produce their final answer at 75% of their step budget.
//!
//! ## Long sessions
//!
//! [`run_with_continuation`] restarts a run that filled its context window
//! from a summary of its transcript, up to
//! [`ContinuationPolicy::max_continuations`] times (default 5).
//!
//! ## Adding an agent
//!
//! In code, build an [`vibe_core::AgentSpec`] and register it:
//!
//! ```
//! use vibe_core::{AgentRole, AgentSpec, Registry, ToolSelection};
//!
//! let mut registry = Registry::new();
//! vibe_agents::register_builtin_agents(&mut registry);
//! registry.add_agent(
//!     AgentSpec::new(
//!         AgentRole::Custom("doc_writer".into()),
//!         "You document {{task_title}} in {{workspace_root}}.",
//!     )
//!     .with_tools(ToolSelection::ReadWrite),
//! );
//! assert_eq!(registry.agents.len(), 13);
//! ```
//!
//! Without code, drop a TOML file in `.vibe/agents/` of the project and load
//! it with [`load_agent_overrides`] or [`apply_agent_overrides`]. A file whose
//! `role` is a built-in role only overrides the fields it sets; any other role
//! defines a new agent and needs a prompt:
//!
//! ```toml
//! role = "doc_writer"                  # built-in role name or any new name
//! description = "Writes documentation"
//! system_prompt_file = "doc_writer.md" # relative to this file; or system_prompt = "…"
//! tools = ["read_file", "write_file"]  # or "none" | "read_only" | "read_write" | "all"
//! model = "anthropic/claude-sonnet-5"  # or "phase" (use the phase model)
//! thinking = "medium"                  # off | low | medium | high | max
//! max_steps = 80
//! max_tokens = 16000
//! structured_output = false
//! ```
//!
//! ## Testing
//!
//! [`test_support`] provides a scripted provider and mock tools so that the
//! loop can be exercised without a network.

#![forbid(unsafe_code)]

pub mod builtin;
pub mod continuation;
pub mod prompts;
pub mod runtime;
pub mod structured;
pub mod test_support;

pub use builtin::{
    apply_agent_overrides, builtin_agent, builtin_agents, load_agent_overrides,
    load_agent_overrides_with_base, project_agents_dir, register_builtin_agents,
};
pub use continuation::{
    ContinuationPolicy, continuation_message, render_transcript, run_with_continuation,
    summarize_transcript,
};
pub use prompts::{builtin_prompt, documented_variables, strip_doc_comment};
pub use runtime::{
    AgentRunner, CONTEXT_WARNING_MESSAGE, CONTINUE_NUDGE, CONVERGE_MESSAGE, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_MAX_TOOL_OUTPUT_CHARS, INVALID_ARGUMENTS_MESSAGE, MAX_RETRY_DELAY, RAW_ARGUMENTS_KEY,
    TRUNCATED_TWICE_MESSAGE,
};
pub use structured::{
    extract_json, invalid_json_message, parse_structured, repair_json, run_structured,
    run_structured_with_hint,
};
