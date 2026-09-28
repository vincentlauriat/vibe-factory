# ADR-008: Trace store and read layer

**Status:** Accepted — 2026-09-27

## Context

Up to 0.4, interfaces show a run while it happens, but reconstructing what was done after
the fact is hard. `tool_returned` carries a 200-character preview and nothing pairs it with
its `tool_called` when read-only tools run in parallel; the complete output of a call is kept
nowhere unless it exceeds the model's limit. Commits other than subtask integrations and the
final merge emit no event, and `run.json`, the only place with a run's totals, is
overwritten by the next run. Each interface that wanted a history, a project-wide activity
feed or a trace of tool calls would have had to re-parse `events.jsonl`, guess, and ask git
on its own.

[ADR-007](007-one-seam-many-interfaces.md) makes events the interface contract and forbids
private views that events cannot rebuild, so whatever these views need must be in the
events, or referenced by them.

## Decision

1. **Events carry what the views need**, as optional fields and new types (schema stays 2):
   a `call` id on `tool_called` and `tool_returned`, the `subtask`, exit code, output length
   and output path on `tool_returned`, the model on `agent_started`, `committed` and `merged`
   events, and the run's totals on `run_finished` ([Events](../../reference/events.md)).
2. **Complete tool outputs go to a trace store**, not into the log:
   `.vibe/tool-output/<task dir>/<run>/<call>.txt` at the project root, ignored by git,
   capped per call (`pipeline.trace_max_chars`), on by default (`pipeline.trace_outputs`),
   removed with the task. `tool_returned.output_file` references the file by a path relative
   to the project root; nothing else points to it.
3. **One read layer in `vibe-pipeline`** turns events and git into views: `events_log`
   (incremental reading of one log, every task's log merged by an event cursor), `history`
   (runs, commits, changed files, validations, QA, tokens, active time and cost of a task)
   and `trace` (the calls of a run, paired, with their outputs). It stores nothing: every
   view is rebuilt from `.vibe/` and git when it is asked for. The command line, the
   terminal UI and the server all use it, so they agree.
4. **The server reads the logs once.** `vibe serve` runs one follower of every task's log,
   shared by all its open event streams, instead of one reader per client.

## Consequences

- `vibe history`, `vibe trace` and `vibe events` without a task, and the matching screens of
  the terminal UI, the web UI and the server routes, are thin renderings of the same
  documents (`TaskHistory`, `RunTrace`, `TaggedEnvelope`).
- The trace store costs disk space in proportion to what the tools printed, bounded per call;
  it is local, git-ignored and can hold secrets that a command printed.
- Logs written before 0.5 still load: their calls pair by order, their outputs are only the
  previews, and their totals are unknown rather than zero.
- `vibe-pipeline` now depends on `vibe-workspace` for read-only git queries of the history;
  the pipeline itself still reaches workspaces through `WorkspaceProvider`.
- Ordering across tasks relies on the clocks of the processes that log; within a task, `seq`
  stays the exact order.
