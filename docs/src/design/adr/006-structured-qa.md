# ADR-006: Structured QA verdicts

**Status:** Accepted — 2026-09-26

## Context

A review loop needs an unambiguous verdict to decide whether to fix again, stop, or ask a
human. Parsing free text ("Status: passed", "LGTM", "approved with nits") is brittle, and
having two sources of truth (a markdown report and a JSON status) invites disagreement.

## Decision

The QA reviewer ends its session with a JSON document that deserialises into
`vibe_core::QaReport` (`verdict`, `summary`, `issues[]` with severity, requirement, file,
line and suggested fix). That document is the only source of truth. The markdown report
is *rendered from it* for humans, never parsed. Invalid JSON goes through a repair call and
one retry; a still-invalid answer counts as `Inconclusive` and consumes a round.

The same rule applies to every structured artefact: complexity assessment, spec, plan.

## Consequences

- The pipeline can reason about severities (block on `High`+, tolerate `Low`).
- Escalation is deterministic: the same issue title three rounds in a row stops the loop.
- Prompts must be explicit about the schema; the runtime helps with extraction and repair.
