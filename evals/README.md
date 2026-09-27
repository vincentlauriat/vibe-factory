# Reproducible evaluations

Ten small Rust projects measure what the pipeline actually delivers. Each case is a
fixture (the project an agent works on), a task statement, and an **independent oracle**:
acceptance tests compiled against the final code from outside the agent's workspace, so a
canned or over-optimistic QA approval never counts as success. Everything needs only
Python 3.8+, Rust and git; no third-party Python or Rust dependency.

| Case | Task | Kind |
| --- | --- | --- |
| `boundary` | Make both bounds of `contains` inclusive | bug fix |
| `slug` | Add a `slug` function and tests | small feature |
| `catalog` | Share one case-insensitive comparison between two modules | refactor across modules |
| `cliargs` | Add `-o/--output`, `-j/--jobs`, `--` and precise errors to an argument parser | spec-heavy feature |
| `config` | Replace panics with a typed `ConfigError` | error handling |
| `csv` | Fix a CSV record splitter (quotes, empty fields, terminators) | bug fix with edge cases |
| `inventory` | Add stock reservations across modules and integration tests | multi-file feature |
| `lru` | Implement an LRU cache from a written contract | data structure |
| `money` | Extract shared money formatting into a new module, no overflow at `i64::MIN` | refactor with edge cases |
| `semver` | Implement `Display`, `FromStr` and `Ord` for a version type | traits and parsing |

## Files

* `cases/<case>.json`: `id`, `title`, `task` (given to the agent), `files` (the fixture) and
  `oracle` (the acceptance tests). Only the fixture and the task reach the agent.
* `references/<case>/`: a reference solution per case, for maintainers. `run.py` never reads
  it; `check_cases.py` uses it to prove each case is solvable.
* `run.py`: one isolated evaluation.
* `run_suite.py`: every case (or a selection), several repetitions, then a summary.
* `summarize.py`: aggregate any set of `report.json` files.
* `check_cases.py`: offline validation of the cases themselves, no model calls.

## Checking the cases

```sh
python3 evals/check_cases.py
```

For every case: the fixture passes its own `cargo test --offline`, the oracle **fails** on
the untouched fixture, and the fixture with its reference solution passes both its tests
and the oracle. Run it after editing a case. `--keep DIR` keeps the logs.

## Smoke test without a model

```sh
cargo build -p vibe-cli
python3 evals/run_suite.py /tmp/vibe-smoke --provider mock --vibe target/debug/vibe
```

The mock does not solve anything: every run reports `success: false`, usually with a
pipeline exit code of 0 (the mock QA approves) and a failing oracle. That distinction is the
point of the oracle.

## Running a real-model baseline

Each run makes billable model calls. Set the provider's API key (and `ANTHROPIC_WORKSPACE_ID`
when an Anthropic key is not scoped to a workspace), then:

```sh
cargo build --release -p vibe-cli
python3 evals/check_cases.py
python3 evals/run_suite.py results/2026-10-01-claude \
    --vibe target/release/vibe \
    --provider anthropic --model anthropic/YOUR_MODEL \
    --repetitions 3 --max-tokens 2000000
```

`--max-tokens` bounds each run (see run budgets in the configuration guide); omit it for
no limit. `--jobs N` runs up to N evaluations at once (each in its own
directory); a full suite takes about two hours with one job. Mind the provider's rate
limits when raising it. Use a new destination for every baseline: the runners never overwrite anything.
The suite writes `summary.md` and `summary.json` next to the per-run directories:

| Column | Meaning |
| --- | --- |
| success | runs whose pipeline exited 0 **and** whose oracle passed |
| mean s, median s | wall time of `vibe run`, in seconds |
| mean tokens | input + output tokens reported by the CLI summary |
| no usage | runs where the CLI failed before reporting usage (excluded from tokens) |
| validations | mean number of required validation executions |

Under the table, `summary.md` lists the pipeline errors of the failed runs, most frequent
first. The suite stops after the first run that fails in a way no other run can avoid
(`AuthFailed`, `InvalidRequest` or `Config`: a missing or rejected API key, an unknown model,
an invalid configuration) and exits with 1. Fix the cause and start again in a new
destination. Runs that last well under a second with 0 tokens never reached the model.

To browse the results, start `vibe serve --evals results` in the project and open the
**Evaluations** view of the web UI.

Record with a published baseline: the framework commit (in every report and the summary),
the Rust toolchain (`rustc --version`), the exact model, the number of repetitions and the
date. Review a sample of runs by hand as well: whether the requested tests were added and
whether the refactoring cases really share code are not checked by the oracles.

Published baselines are in [`baselines/`](baselines/):

* [Claude Sonnet 5, 2026-09-27](baselines/2026-09-27-claude-sonnet-5.md) (partial: 16 runs
  over six cases): 16/16 successful, about four minutes and 150 000 tokens per run.

## One run

```sh
python3 evals/run.py lru /tmp/vibe-eval-lru --vibe target/debug/vibe --provider mock
```

The runner creates an isolated git repository and uses an in-place pipeline with one coder
at a time. Mandatory `cargo test --offline` checks run before ready. The source fixture must
build and its oracle must initially fail. Each subprocess is limited to 30 minutes; a timeout
is reported as exit code 124. This is an evaluation harness, not a process or container
sandbox.

Artifacts in the destination:

* `project/`: task workspace and `.vibe/tasks/*/{run.json,progress.md,events.jsonl}`;
* `oracle/`: acceptance tests outside the agent workspace;
* `fixture.log`, `baseline.log`, `acceptance.log`: build and acceptance evidence;
* `run.jsonl`: CLI events and summary, and any stderr diagnostics;
* `report.json`: the result.

`report.json` (`schema_version` 1) holds the case, provider, model override, wall time,
token usage, pipeline and oracle exit codes, validation records and their count, the `vibe`
version and framework commit (`framework`), and `success`, which requires both a successful
pipeline exit and a passing oracle. `estimated_cost` is null: no pricing assumptions are
made. `usage` is null if the CLI failed before producing its summary.
`human_interventions` is zero because the runner performs one unattended invocation.
