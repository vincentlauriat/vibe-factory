# Events

Everything the engine does is published as an event. Interfaces (the CLI, `vibe events`,
the terminal UI, and later `vibe serve`) are built on events only, so this page is a
public, versioned contract ([ADR-007](../design/adr/007-one-seam-many-interfaces.md)).

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
| `agent_started` | `run`, `role`, `subtask` | an agent session starts |
| `agent_delta` | `run`, `role`, `subtask`, `delta` (`{"kind":"text"\|"thinking","text":…}`) | **ephemeral**: text arriving while a step streams |
| `agent_text` | `run`, `role`, `text` | the complete text of a step |
| `tool_called` | `run`, `role`, `tool`, `input` | before a tool runs |
| `tool_returned` | `run`, `role`, `tool`, `is_error`, `duration_ms`, `preview` | after a tool ran |
| `agent_finished` | `run`, `role`, `steps`, `usage`, `stop` | an agent session ends |
| `subtask_updated` | `run`, `subtask`, `status` | a subtask changes state |
| `subtask_integrated` | `run`, `subtask`, `commit`, `conflicts` | a finished attempt was merged into the task branch (`conflicts` empty) or conflicted |
| `validation_finished` | `run`, `command`, `integration`, `passed`, `exit_code` | a required validation command finished |
| `budget_updated` | `run`, `tokens`, `token_limit`, `active_ms`, `duration_limit_ms` | after each phase and each subtask attempt |
| `artefact_written` | `run`, `artefact` (`{"kind":"spec"}`, `{"kind":"plan"}`, `{"kind":"qa_report","round":n}`) | the spec, the plan or a QA report was written |
| `approval_requested` | `run`, `gate` (`spec`, `plan`, `merge`) | the run pauses until a human decides (`pipeline.approvals`) |
| `approval_resolved` | `run`, `gate`, `approved`, `comment` | `vibe approve` or `vibe reject` answered; logged by that command |
| `retrying` | `run`, `what`, `attempt`, `delay_ms` | before a retry |
| `paused` | `run`, `reason` | the run stops and waits for a human |
| `run_finished` | `run`, `success`, `status` | the run ends, always last, also after `paused` |
| `log` | `run` (optional), `level`, `message` | diagnostics |

Identifiers (`run`, `task`, `subtask`) are UUIDs. `role` is the agent role (`coder`,
`qa_reviewer`, …). Statuses and phases use the same snake case names as `vibe task show`.
