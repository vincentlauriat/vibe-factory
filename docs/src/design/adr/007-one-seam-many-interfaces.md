# ADR-007: One seam for every user interface

**Status:** Accepted — 2026-09-27

## Context

Up to 0.2 the only interface is the `vibe` command line: `vibe run` builds a pipeline in
its own process, prints events as they come, and the run ends with the process. A terminal
UI, a local web UI and an editor extension are all wanted. Each needs the same things:
start, resume and cancel runs; see live progress, including text as the model produces it;
answer approval requests; reconnect to a run in progress without losing events.

Building each interface directly on `Pipeline` would duplicate run management three times,
and each would have to guess the state of a run from files on disk.

## Decision

1. The engine exposes one seam for interfaces: a **run manager** in `vibe-pipeline` that
   starts, resumes, cancels and lists runs and hands out an event subscription. It never
   renders anything.
2. **Events are the interface contract.** Every change an interface shows is an event with
   a per-run sequence number and a schema version, persisted in `events.jsonl`, so a client
   can replay from any sequence number and then follow live events. Interfaces keep no
   private view of engine state that events cannot rebuild.
3. **Human decisions are part of the pipeline**, not of an interface: an approval request
   is persisted in `run.json` and resolved by a command (`vibe approve`, `vibe reject`)
   that any interface can issue.
4. Clients attach in two ways: in process (the CLI and the terminal UI use the run manager
   directly) or through `vibe serve`, which exposes the same operations over HTTP and the
   event stream over server-sent events (web UI, editor extensions).
5. `.vibe/` stays the source of truth and is protected by a cross-process lock, so a CLI
   and a server can work on the same project.

## Consequences

- The terminal UI can ship before the server, and the server adds no engine logic.
- Streaming, approvals and new events must land before any interface that relies on them.
- The event schema becomes a public, versioned API; changing an event is a breaking change.
- A crash loses no decision: approvals and sequence numbers are on disk.
