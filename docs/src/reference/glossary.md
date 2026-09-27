# Glossary

| Term | Definition |
|------|------------|
| **Agent** | A model running in a loop with tools, described by an `AgentSpec`. |
| **AgentSpec** | Declarative agent definition: role, system prompt, tools, model, thinking level, step budget. |
| **Artefact** | A file produced by a phase (`spec.md`, `plan.json`, `qa_report.json`, …). |
| **Base branch** | The branch worktrees are created from and merged into. |
| **Complexity** | `trivial`, `simple`, `standard` or `complex`; selects the pipeline profile. |
| **Event** | A structured notification published on the `EventBus` (phase started, tool called, …). |
| **Hook** | Extension that observes or vetoes phases and tool calls and can enrich prompts. |
| **Memory** | Knowledge retained across runs (gotchas, patterns, decisions). |
| **Phase** | One stage of the pipeline: `assess`, `spec`, `plan`, `build`, `qa`, `fix`, `merge`. |
| **Pipeline** | The state machine that drives a task through its phases. |
| **Plugin** | A bundle of extensions, in-process (Rust) or out-of-process (any language). |
| **Profile** | The list of phases chosen for a complexity class. |
| **Provider** | A model back-end (`ModelProvider`), e.g. Anthropic, OpenAI-compatible, mock. |
| **QA report** | Structured verdict of the reviewer: `approved`, `changes_requested` or `inconclusive`, with issues. |
| **Registry** | The collection of every provider, tool, agent, workspace, memory store and hook available to a run. |
| **Run** | One execution of the pipeline on a task, identified by a `RunId`. |
| **Spec** | Specification: summary, requirements with acceptance criteria, gathered context. |
| **Subtask** | Smallest unit of implementation work, executed by one coder session. |
| **Task** | The unit of work handed to the framework. |
| **Thinking level** | Reasoning budget requested from the model: `off`, `low`, `medium`, `high`, `max`. |
| **Tool** | A capability an agent may invoke (read a file, run a command, …). |
| **VPP** | Vibe Plugin Protocol: JSON-RPC 2.0 over stdio, MCP-shaped. |
| **Workspace** | The directory a task's agents are confined to; a git worktree by default. |
