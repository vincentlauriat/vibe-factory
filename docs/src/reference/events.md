# Events

Everything the engine does is published as an event. Interfaces (the CLI, `vibe events`,
`vibe history`, `vibe trace`, the terminal UI, `vibe serve` and its clients) are built on
events only, so this page is a public, versioned contract
([ADR-007](../design/adr/007-one-seam-many-interfaces.md),
[ADR-008](../design/adr/008-trace-store-and-read-layer.md)).

## Envelope

Each event travels in an envelope, one JSON object per line in
`.vibe/tasks/<task>/events.jsonl` and in `vibe --json` output:

```json
{"schema":2,"seq":17,"at":"2026-09-27T10:00:00Z","event":{"type":"phase_started","run":"…","phase":"build"}}
```

| Field | Meaning |
|-------|---------|
| `schema` | event format version, currently `2`; logs written before 0.3 have none and read as `1` |
| `seq` | position of the event in its run, from 1, without gaps and continued across resumes; absent for ephemeral events and events without a run |
| `at` | publication time, UTC |
| `event` | the event, tagged by `type` |

A client replays a run by reading its events in `seq` order and follows it by asking for the
events after the last `seq` it saw: `vibe events <task> --after <seq> --follow`. Events of
one run are delivered in `seq` order to live subscribers and to the log.

**Ephemeral** events are only sent live and never logged: they carry no `seq` and a client
that reconnects does not get them back.

Adding an event type or an optional field is not a breaking change; clients must ignore
types and fields they do not know. Removing or changing a field increments `schema`.

## Event types

| `type` | Fields | When |
|--------|--------|------|
| `run_started` | `run`, `task` | a run or a resumed run starts |
| `phase_started` | `run`, `phase` | before a phase |
| `phase_finished` | `run`, `phase`, `success`, `summary` | after a phase |
| `agent_started` | `run`, `role`, `subtask`, `model` | an agent session starts; `model` is the provider's model id |
| `agent_delta` | `run`, `role`, `subtask`, `delta` (`{"kind":"text"\|"thinking","text":…}`) | **ephemeral**: text arriving while a step streams |
| `agent_text` | `run`, `role`, `text` | the complete text of a step |
| `tool_called` | `run`, `role`, `tool`, `input` (complete arguments), `call`, `subtask` | before a tool runs |
| `tool_returned` | `run`, `role`, `tool`, `is_error`, `duration_ms`, `preview` (first 200 characters), `call`, `subtask`, `exit_code`, `timed_out`, `output_chars`, `output_file` | after a tool ran |
| `agent_finished` | `run`, `role`, `steps`, `usage`, `stop` | an agent session ends |
| `subtask_updated` | `run`, `subtask`, `status` | a subtask changes state |
| `subtask_integrated` | `run`, `subtask`, `commit`, `conflicts` | a finished attempt was merged into the task branch (`conflicts` empty) or conflicted |
| `committed` | `run`, `subtask`, `commit`, `message`, `files` | the pipeline committed in the task workspace: checkpoint before the build, subtask or fix commit, subtask integration |
| `merged` | `run`, `commit`, `branch`, `base` | the merge phase merged the task branch into its base (`auto_merge`); `commit` is the resulting commit of `base`; `branch` or `base` is `""` when the workspace provider does not know it |
| `validation_finished` | `run`, `command`, `integration`, `passed`, `exit_code` | a required validation command finished |
| `budget_updated` | `run`, `tokens`, `token_limit`, `active_ms`, `duration_limit_ms` | after each phase and each subtask attempt |
| `artefact_written` | `run`, `artefact` (`{"kind":"spec"}`, `{"kind":"plan"}`, `{"kind":"qa_report","round":n}`) | the spec, the plan or a QA report was written |
| `approval_requested` | `run`, `gate` (`spec`, `plan`, `merge`) | the run pauses until a human decides (`pipeline.approvals`) |
| `approval_resolved` | `run`, `gate`, `approved`, `comment` | `vibe approve` or `vibe reject` answered; logged by that command |
| `retrying` | `run`, `what`, `attempt`, `delay_ms` | before a retry |
| `paused` | `run`, `reason` | the run stops and waits for a human |
| `run_finished` | `run`, `success`, `status`, `usage`, `active_ms`, `started_at` | the run ends: always the last event of a run that published `run_started`, also after `paused` and after an error (then `success` is `false`, with `status` `failed`, or the unchanged task status when the workspace could not be opened); totals of the whole run, resumes included |
| `log` | `run` (optional), `level`, `message` | diagnostics |

Identifiers (`run`, `task`, `subtask`) are UUIDs. `role` is the agent role (`coder`,
`qa_reviewer`, …). Statuses and phases use the same snake case names as `vibe task show`.
`usage` is `{"input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"}`.

### Tool calls

`call` is a short random id (12 hex digits) generated for each call: it pairs a
`tool_called` with its `tool_returned`, including when read-only calls of one step run in
parallel and return out of order. `subtask` is the subtask the session works on, `null`
outside the build.

On `tool_returned`, `exit_code` and `timed_out` come from tools that run a command (`bash`);
other tools leave them `null` and `false`. `output_chars` is the length of the complete
output, before any truncation. `output_file` points to the complete output (see below), or
is `null` when tracing is off or the file could not be written.

### Trace store

With `pipeline.trace_outputs = true` (the default), the complete output of every tool call
is kept in `.vibe/tool-output/<task dir>/<run>/<call>.txt`, where `<task dir>` is the task's
directory name under `.vibe/tasks/` (`003-add-login`). `output_file` gives that path
relative to the project root. Each file holds at most `pipeline.trace_max_chars` characters
(100 000 by default); a longer output is cut and ends with a marker line. The directory is
ignored by git (`.vibe/.gitignore`, written by `vibe init`) and removed by
`vibe task discard`. It is independent of what the model sees: an output longer than the
runner's limit is still truncated for the model, with the complete text in
`<workspace>/.vibe/tool-output/<uuid>.txt` where the agent can read it.

### Commits

`committed.subtask` is `null` for the checkpoint before the build, for fix commits, and for
one commit shared by several subtasks when subtasks are not isolated. `committed.files`
lists the paths changed by the commit against its first parent (for a merge commit, what it
brought in), relative to the repository root; it is empty when the committer does not use
git. `message` is the first line of the commit message. Commits the git workspace makes on
its own are not announced: the checkpoint just before merging and the commit of an assisted
conflict resolution; the merge itself is, by `merged`.

### Logs recorded before 0.5

Every field added in 0.5 is optional when reading, so older logs still load under schema
`2`: `call` reads as `000000000000` (pair those calls by order and `tool`), `subtask`,
`exit_code` and `output_file` as `null`, `timed_out` as `false`, `output_chars` as `0`,
`model` as `""`, and `run_finished` without totals reads `usage` as zeros, `active_ms` as
`0` and `started_at` as `1970-01-01T00:00:00Z`.

## Events of every task

A task's `events.jsonl` does not name its task: it is implied by the directory. Views over
the whole project (`vibe events` without a task, `GET /api/events`, `GET /api/stream`, the
Activity screens) read every task's log and tag each envelope with its task, serialised
flat, the envelope's fields next to the tag:

```json
{"task":"6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19","number":3,"schema":2,"seq":17,"at":"2026-09-27T10:00:00Z","event":{"type":"phase_started","run":"…","phase":"build"}}
```

`task` is the task id and `number` its number on the board. This shape is
`vibe_pipeline::TaggedEnvelope`.

Tagged events are ordered by an **event cursor**: the time `at`, then the task number, then
`seq` (`0` when absent). It is written `<nanoseconds since the epoch>-<number>-<seq>`, for
example `1790000000123456789-3-17`, and serves as the id of the events of `/api/stream`. A
client that resumes after the cursor of the last event it saw gets neither a repeat nor a
loss of events of other tasks logged at the same instant; `0-0-0` means "from the start".

The order across tasks is the order of the `at` times, and each process stamps its own
events. Events of one task always come in order, but a `vibe run` in another process whose
clock reads slightly earlier can log an event whose cursor sorts before one already
delivered for another task. A live follower still delivers it, once, so cursors are not
always increasing on the wire; a client that was disconnected while it was logged and
resumes from a later cursor does not get it. When the state of one task must be exact,
reload that task's events (`vibe events <REF>`, `GET /api/tasks/{ref}/events`), which are
ordered by `seq`.
