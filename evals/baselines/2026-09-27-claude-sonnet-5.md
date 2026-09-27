# Baseline: Claude Sonnet 5, 2026-09-27

First real-model run of the suite: ten cases, three repetitions each, 30 runs. It ran in two
sessions: 16 runs one after the other, then the 14 remaining ones (`lru`, `money`, `semver`,
`slug` and two `inventory` runs) with `run_suite.py --jobs 3`.

| Setting | Value |
| --- | --- |
| Date | 2026-09-27 |
| Framework commits | `e404624e2b6dd5c6d91336dbf18e0121cc497a51` (16 runs), `88bc6eccf3461f509dd0a26e2c1e71604a29ff78` (14 runs); the crates differ only by a lint fix in `forge.rs`, outside the run path |
| Toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, macOS |
| Provider and model | `anthropic`, `anthropic/claude-sonnet-5` for every phase |
| Thinking | adaptive, effort from each agent's thinking level |
| Token budget | `--max-tokens 2000000` per run |
| Command | `python3 evals/run_suite.py … --provider anthropic --model anthropic/claude-sonnet-5 --repetitions 3 --max-tokens 2000000 [--jobs 3 --cases …]` |

| provider | model | case | runs | success | mean s | median s | mean tokens | no usage | validations |
|---|---|---|---|---|---|---|---|---|---|
| anthropic | anthropic/claude-sonnet-5 | boundary | 3 | 3/3 | 73.1 | 64.7 | 27618 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | catalog | 3 | 3/3 | 261.4 | 231.5 | 128146 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | cliargs | 3 | 3/3 | 291.2 | 319.2 | 220512 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | config | 3 | 3/3 | 205.4 | 199.3 | 201765 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | csv | 3 | 3/3 | 247.8 | 202.0 | 135821 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | inventory | 3 | 3/3 | 498.3 | 522.0 | 216122 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | lru | 3 | 3/3 | 274.5 | 269.6 | 115665 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | money | 3 | 3/3 | 173.3 | 166.1 | 84177 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | semver | 3 | 3/3 | 151.2 | 132.5 | 96789 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | slug | 3 | 3/3 | 147.2 | 110.4 | 49992 | 0 | 1 |
| anthropic | anthropic/claude-sonnet-5 | ALL | 30 | 30/30 | 232.3 | 201.5 | 127661 | 0 | 1 |

Every run passed its oracle, with one required validation run each. A run takes about four
minutes and 130 000 tokens on average; `boundary` is the cheapest case, `inventory` the most
expensive. The runs made with three jobs at once took no longer than the sequential ones.

## Review by hand

The oracles check behaviour only. The final workspace of every run was compared with its
baseline commit for what they miss:

* Tests: every run added tests (7 to 24 `#[test]` functions, 13 on average) and none
  removed an existing one. `inventory` runs also added an integration test file.
* `money`: in the three runs, `invoice_line` and `refund_line` are one line each, a call to
  `crate::money::format_cents`; no formatting code is left in either module.
* `catalog`: the three runs moved the comparison into a separate module used by both `find`
  and `search`, but only one has a single helper. In the two others the module holds two
  functions, an equality for `find` and a substring test for `search`, with the same ASCII
  semantics but independent code (one uses `to_ascii_lowercase`, the other byte windows).
  The task asks for "the same comparison helper", so these two runs meet the letter of the
  oracle, not quite the intent. The case could say that `search` must be built on the
  helper `find` uses.
