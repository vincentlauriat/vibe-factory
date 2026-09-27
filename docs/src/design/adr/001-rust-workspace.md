# ADR-001: Rust and a cargo workspace

**Status:** Accepted — 2026-09-26

## Context

An autonomous coding framework runs for hours, spawns processes, talks to several network
services concurrently and manipulates the user's repository. It must be fast to start,
cheap to run on a laptop, safe against memory errors, and distributable as a single binary
on Linux, macOS and Windows. It must also be pleasant to extend by people who are not its
authors.

## Decision

The framework is written in Rust (edition 2024, MSRV 1.88) and organised as a cargo
workspace of small, layered crates. Each crate has one responsibility and a documented
public API; `vibe-core` holds only types and traits and has no I/O dependencies.

`unsafe_code` is forbidden workspace-wide, every public item is documented
(`missing_docs = warn` with `-D warnings` in CI), and clippy runs on all targets.

## Consequences

- Single static binary, no runtime to install, fast start-up.
- Strong typing of every artefact (spec, plan, report) catches malformed model output at
  the boundary instead of deep inside the pipeline.
- Async everywhere via Tokio; agents and tools run concurrently without threads per task.
- Higher barrier for casual contributors than a scripting language; mitigated by the
  out-of-process plugin protocol ([ADR-005](005-plugin-protocol.md)) that lets extensions
  be written in any language.
