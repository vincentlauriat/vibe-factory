# Reproducible evaluations

These three small Rust projects establish the first behavioral baseline for v0.2.
They require Python 3, Rust and git, with no third-party Python or Rust dependencies.

| Case | Task | Independent acceptance checks |
| --- | --- | --- |
| `boundary` | Correct an inclusive upper bound | Edges, singleton range, reversed range, outside values |
| `slug` | Add a function and regression tests | ASCII normalization, separators, empty and Unicode input, existing API |
| `catalog` | Coordinate lookup changes across modules | Exact and substring lookup, case handling, order, empty input |

Build the CLI, then run a smoke evaluation in a **new** directory:

```sh
cargo build -p vibe-cli
python3 evals/run.py boundary /tmp/vibe-eval-boundary --vibe target/debug/vibe --provider mock
```

The mock does not solve the fixture: exit code 1 and `success: false` are expected.
The pipeline can report ready while the independent oracle fails; this distinction is
intentional and prevents a canned QA approval from being counted as evaluation success.

For a real evaluation, set the provider's API key in the environment and replace `mock`
with the provider and an explicit supported model, for example using
`--provider anthropic --model anthropic/YOUR_MODEL`. Each run makes billable model calls.
Use a fresh destination for every case/model/repetition. The runner never overwrites a run.

The runner creates an isolated git repository and uses an in-place pipeline with one coder
at a time. Mandatory `cargo test --offline` checks run before ready. An external oracle crate
then tests the final public API, independently of agent-edited tests. The source fixture must
build and its oracle must initially fail. Each subprocess is limited to 30 minutes; a timeout
is reported as exit code 124. This is an evaluation harness, not a process/container sandbox.

Artifacts in the destination:

- `project/`: task workspace and `.vibe/tasks/*/{run.json,progress.md,events.jsonl}`.
- `oracle/`: acceptance tests outside the agent workspace.
- `fixture.log`, `baseline.log`, `acceptance.log`: build and acceptance evidence.
- `run.jsonl`: CLI events/summary and any stderr diagnostics.
- `report.json`: versioned result with case, provider, model override, wall time, token usage,
  pipeline/oracle exit codes, validation attempts and overall behavioral success.

`schema_version` is currently 1. `success` requires both a successful pipeline exit and a
passing oracle. `estimated_cost` is null: no pricing assumptions are made. Usage is null
if the CLI failed before producing its summary. `human_interventions` is zero because the
runner performs one unattended invocation; it does not resume or edit the result.

These are behavioral checks, not a full quality score. Manually review whether requested
tests were added and whether the catalog refactoring actually shares a helper. A follow-up
baseline should record the framework commit, toolchain, exact model and repeated runs, and
expand to ten cases before publishing any reliability claims. No live-model scores have
been collected in this increment.
