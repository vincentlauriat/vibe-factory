# Customising agents

Every step of the pipeline is carried out by an **agent**: a model running in a loop with a
system prompt, a set of tools, a model choice and a thinking level. Vibe Factory ships twelve
of them. You can change any of their settings, replace their prompts, or add your own, by
dropping TOML files in `.vibe/agents/`. No code is needed.

## The built-in roles

| Role | What it does |
|------|--------------|
| `complexity_assessor` | Reads the task and a little of the code, then classifies it as trivial, simple, standard or complex. This picks how much of the pipeline runs. |
| `spec_gatherer` | Explores the codebase and turns your description into numbered, testable requirements with the relevant files and conventions. |
| `spec_researcher` | Checks the external libraries and APIs the requirements rely on, so the spec does not assume functions that do not exist. |
| `spec_writer` | Writes the specification: a readable `spec.md` and the matching JSON. |
| `spec_critic` | Reviews the specification for gaps and contradictions and returns a corrected version. |
| `planner` | Breaks the spec into ordered phases of small subtasks, each with the files to touch, its dependencies and how to verify it. |
| `coder` | Implements one subtask in a fresh session, then runs its verification. |
| `coder_recovery` | Takes over a subtask that keeps failing, diagnoses why, and completes it another way. |
| `qa_reviewer` | Checks every acceptance criterion against the code, runs the checks, and issues a verdict with a list of issues. |
| `qa_fixer` | Fixes the issues listed in a QA report. |
| `merge_resolver` | Resolves the conflict markers of one file. |
| `commit_message` | Writes a conventional commit message for a set of changes. |

Their default settings:

| Role | Tools | Thinking | Ends with JSON | Max steps |
|------|-------|----------|----------------|-----------|
| `complexity_assessor` | read only | low | yes | 30 |
| `spec_gatherer` | read only | medium | yes | 100 |
| `spec_researcher` | read only | medium | no | 100 |
| `spec_writer` | read + `write_file` | high | yes | 100 |
| `spec_critic` | read only | high | yes | 60 |
| `planner` | read only | high | yes | 100 |
| `coder` | all | low | no | 300 |
| `coder_recovery` | all | medium | no | 300 |
| `qa_reviewer` | all | high | yes | 200 |
| `qa_fixer` | all | medium | no | 200 |
| `merge_resolver` | none | low | no | 3 |
| `commit_message` | none | low | no | 2 |

All of them use the model configured for the current phase (see
[Providers and models](providers.md#models-per-phase)).

## Overriding a role

Create `.vibe/agents/<anything>.toml`. The file names the role it applies to and sets only
the keys you want to change; everything else keeps its current value.

```toml
# .vibe/agents/qa_reviewer.toml
role = "qa_reviewer"
model = "anthropic/claude-opus-5"
thinking = "max"
max_steps = 250
```

| Key | Type | Meaning |
|-----|------|---------|
| `role` | string, required | a built-in role name, or a new name for a custom agent |
| `description` | string | shown in listings |
| `system_prompt` | string | the whole system prompt, replacing the current one |
| `system_prompt_file` | path | read the prompt from a file, relative to the TOML file |
| `tools` | `"none"`, `"read_only"`, `"read_write"`, `"all"`, or a list of tool names | tools the agent may use |
| `model` | `"phase"` or `"provider/model"` | `phase` follows the phase configuration; a reference pins one model |
| `thinking` | `off`, `low`, `medium`, `high`, `max` | reasoning budget |
| `max_steps` | integer ≥ 1 | maximum number of model round trips |
| `max_tokens` | integer ≥ 1 | output token budget per round trip |
| `structured_output` | bool | the agent must end with a JSON document |

Rules:

- Unknown keys are an error, so a typo such as `max_step` is reported instead of ignored.
- `system_prompt` and `system_prompt_file` are mutually exclusive.
- A tool list only keeps tools that exist; an unknown name is silently dropped. `read_only`
  means `read_file`, `glob`, `grep` and `list_dir`; `read_write` adds `write_file`; `all`
  includes tools from plugins.
- `model` must contain a `/` unless it is `phase`.
- Errors name the file: ``.vibe/agents/qa.toml: TOML error: unknown field `max_step` …``.

### Precedence

Files are read in file-name order. Each file applies on top of the agent already known for
its role: the built-in agent, an agent provided by a plugin, or an earlier file in the same
directory. Two files for the same role therefore layer, the later one winning on the keys
both set. Keep one file per role unless you have a reason not to.

### Replacing a prompt

Copy the built-in prompt from `crates/vibe-agents/src/prompts/<role>.md` as a starting point
and point `system_prompt_file` at your copy:

```toml
# .vibe/agents/coder.toml
role = "coder"
system_prompt_file = "prompts/coder.md"   # .vibe/agents/prompts/coder.md
```

Prompts are templates: `{{name}}` is replaced by a variable, and unknown variables become
empty text. Every agent gets `{{task_title}}`, `{{task_description}}`, `{{workspace_root}}`
and `{{date}}`. The built-in prompts start with an HTML comment listing the other variables
their role receives (`{{spec}}`, `{{plan}}`, `{{subtask}}`, `{{progress}}`, `{{memory}}`,
`{{qa_report}}`, `{{prior_context}}`, …); that comment is stripped before the prompt reaches
the model, so you can keep it as documentation. The full table is in
[Agent runtime](../design/agent-runtime.md#prompt-variables).

If a role ends with JSON, your prompt must still ask for the same JSON shape: the pipeline
parses it into typed data.

## Adding a custom role

Any `role` that is not a built-in name defines a new agent. It must provide a prompt; the
other keys default to all tools, the phase model, `medium` thinking, 200 steps and 8 192
output tokens.

```toml
# .vibe/agents/doc_writer.toml
role = "doc_writer"
description = "Keeps the user guide in sync with the code"
system_prompt_file = "doc_writer.md"
tools = ["read_file", "glob", "grep", "write_file"]
thinking = "medium"
max_steps = 80
```

```markdown
<!-- .vibe/agents/doc_writer.md -->
You maintain the documentation of the project at {{workspace_root}}.
Task: {{task_title}}

{{task_description}}

Update only files under docs/. Finish with a short list of the pages you changed.
```

The default pipeline drives the built-in roles; a custom role is available to plugins and
programs that look agents up by name in the registry. Plugins can also contribute agents
over the plugin protocol (see [Plugins](plugins.md)).

## Writing prompts that return valid JSON

Agents with `structured_output = true` must end their session with one JSON document. The
runtime is tolerant, but these habits keep the retries (and their cost) away:

- **Show the exact shape** in the prompt, as a fenced `json` block with every key, and list
  allowed enum values literally (`"approved | changes_requested | inconclusive"`). Enum values
  are `snake_case` and must match exactly.
- **Ask for the JSON last**, in a single `json` fence, with nothing after it. The extractor
  takes the whole answer if it is pure JSON, else the *last* `json` fence, else the last
  untagged fence, else the last balanced `{…}` or `[…]` in the text. An example block quoted
  earlier in the answer is harmless; a second document after the real one is not.
- **Objects or arrays only.** A bare string or number is never accepted as the answer.
- **No comments, no trailing commas.** They are the usual reason for the repair step.
- **Give enough output budget.** Plans and specs are long; the built-ins use 32 000 tokens.
  If an answer is cut by the output limit, the agent is asked once to continue, which can
  split the JSON across two messages.
- **Let it converge.** Structured agents are told "Converge now" at 75 % of `max_steps`;
  keep `max_steps` large enough that exploration fits before that point.

When extraction fails, the runtime makes one cheap repair call without tools, then resumes
the agent once with "Your previous answer was not valid JSON". A second failure is reported
as an error; for the QA reviewer it counts as an inconclusive round.
