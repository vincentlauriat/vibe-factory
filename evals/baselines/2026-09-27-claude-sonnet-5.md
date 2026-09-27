# Baseline: Claude Sonnet 5, 2026-09-27 (partial)

First real-model run of the suite. It was stopped after 16 of the planned 30 runs: six of
the ten cases, three repetitions each except `inventory` (one). `lru`, `money`, `semver`
and `slug` were not run.

| Setting | Value |
| --- | --- |
| Date | 2026-09-27 |
| Framework commit | `e404624e2b6dd5c6d91336dbf18e0121cc497a51` |
| Toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, macOS |
| Provider and model | `anthropic`, `anthropic/claude-sonnet-5` for every phase |
| Thinking | adaptive, effort from each agent's thinking level |
| Token budget | `--max-tokens 2000000` per run |
| Command | `python3 evals/run_suite.py … --provider anthropic --model anthropic/claude-sonnet-5 --repetitions 3 --max-tokens 2000000` |

| provider | model | case | runs | success | mean s | median s | mean tokens | no usage | validations |
|---|---|---|---|---|---|---|---|---|---|
| anthropic | anthropic/claude-sonnet-5 | boundary | 3 | 3/3 | 73.1 | 64.7 | 27618 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | catalog | 3 | 3/3 | 261.4 | 231.5 | 128146 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | cliargs | 3 | 3/3 | 291.2 | 319.2 | 220512 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | config | 3 | 3/3 | 205.4 | 199.3 | 201765 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | csv | 3 | 3/3 | 247.8 | 202.0 | 135821 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | inventory | 1 | 1/1 | 524.9 | 524.9 | 226158 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | ALL | 16 | 16/16 | 235.1 | 210.0 | 147984 | 0 | 1 |

Every completed run passed its oracle, with one required validation run each. A run takes
about four minutes and 150 000 tokens on average; `boundary` is the cheapest case,
`inventory` the most expensive.

Not yet done: the four remaining cases, three repetitions of `inventory`, and the review by
hand of a sample of runs (whether the requested tests were added, whether the refactoring
cases really share code), which the oracles do not check.
