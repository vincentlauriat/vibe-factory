# Step 6 report: docs, landing pages and release notes

Branch `feat/visibility-0.5`, main checkout. Nothing committed. I touched only docs/**, README.md,
CHANGELOG.md, ARCHITECTURE*.md and editors/vscode/README.md. I did not touch apps/, crates/,
.github/, PLAN.md, TODOS.md, COMMANDS.md or CHANGES.md; the apps/macos changes in `git status`
come from step 7b.

Examples in the new pages are real output. I got them with `target/debug/vibe`, built from this
checkout, on a scratch git project: one mock task, and one scripted task using `write_file` and a
failing `bash`, both with `--auto-merge`.

## Files changed

### New
- `docs/src/user/history.md`. What `vibe history` shows, with the table and the detail.
  - Where each figure comes from: events, `run.json`, QA reports and git.
  - The five sources of changed files and the `approximate` marker.
  - `+`, `~` and `-`, and tokens, time and cost with `[pricing]`.
  - JSON output, and the API, web, TUI and macOS equivalents.
- `docs/src/user/trace.md`. `vibe trace` and its options, with a real block.
  - The trace store: path, `trace_outputs` and `trace_max_chars`, the cut marker, and the second copy in the workspace for the model.
  - Disk usage and how to reclaim it.
  - Privacy: the store is git-ignored, but `tasks/*/events.jsonl`, which is committed by default, holds complete arguments and previews. `vibe serve` serves outputs to anyone holding the token.
  - Logs from before 0.5, and the JSON, API, web and TUI equivalents.
- `docs/src/design/adr/008-trace-store-and-read-layer.md`. Context, decision and consequences:
  - events reference the outputs;
  - the outputs live under `.vibe/tool-output`;
  - the read layer rebuilds views from events and git, and stores nothing;
  - the server runs one shared follower.

### User guide
- `SUMMARY.md`: History and Trace pages after the CLI reference, and ADR-008 under the ADRs.
- `introduction.md`, the Pages landing page:
  - the pitch now covers seeing what was done;
  - new bullets: four interfaces on one engine, "See what was done", human control;
  - links to History and Trace.
- `quickstart.md`:
  - new step "8. See what was done": `vibe history`, `vibe history 1`, `vibe trace 1 [--full]`, `vibe events --since 1d` and `--follow`, `vibe tui`, `vibe serve` views, the macOS app;
  - step 7 no longer runs `vibe task discard` right after the merge, because discard deletes the task, and with it the history and trace used in step 8. It now explains what discard removes;
  - Next steps links to History and Trace.
- `concepts.md`: new section "Events, trace and history", which covers events as the contract, the trace store and the read layer.
- `configuration.md`:
  - the `trace_outputs` path is fixed (`<task dir>`, not `<task>`), with a link to the Trace page;
  - `trace_max_chars` states that the model's truncation is separate;
  - the top-level table gains the missing `[workspace.container]` and `[integrations]` rows;
  - `[pricing]` says what prices are used for, and that a local model needs a zero price, with an example;
  - the complete example shows the two trace keys.
- `cli.md`, read whole:
  - the synopsis lists every command; `approve`, `reject`, `cancel`, `pr`, `memory`, `tui`, `serve` and `task import` were missing;
  - `--json` mentions `vibe events`;
  - `task discard` removes `.vibe/tool-output/<task dir>/`;
  - `events --follow` on resumed runs, and which events render only with `-v` (checked in `render.rs`);
  - history and trace link to their pages;
  - `--exit-on-stdin-eof` names the macOS app;
  - new "Exit codes" section: 0 and 1 in general, 2 for usage errors (checked: `--type bogus` gives 2, an unknown ref gives 1), and the specific cases of `vibe run`, `doctor` and closed pipes.
  - The API table was already complete from step 4.
- `migration.md`: new first section, "From 0.4 to 0.5".
  - Nothing to convert, schema 2, how 0.4 logs read, and `Task.branch`.
  - Defaults that change: the trace store and its footprint, `events --follow` on resumed runs, the web UI's single stream, 400 for an ambiguous ref, TUI screens and `Esc`.
  - New settings, commands and routes, and the macOS app.
  - Library users: `Event` variants and fields, `CallId`, `Task`/`PipelineConfig`/`VibeConfig` fields, `ToolTrace`, `RunContext::commit(…, subtask)`, the three new `PipelineStore` methods, the read layer modules, and the `vibe-workspace` dependency. I derived this list from `git diff v0.4.0.. -- crates/*/src`.
- `troubleshooting.md`:
  - three rows in "Where to look" (`vibe history`, `vibe trace`, `vibe events --since`), plus a `vibe trace --tool bash --full` example;
  - four new sections:
    - `vibe` takes the home directory for the project, because of another tool's `~/.vibe`;
    - `.vibe/tool-output/` keeps growing;
    - an orphan `vibe serve`, fixed with `--exit-on-stdin-eof`; the error text is the real one (`cannot listen … caused by: Address already in use (os error 48)`);
    - `vibe history` shows `~`, `+` or `-`.

### Reference and design
- `reference/events.md`:
  - the intro no longer says "and later `vibe serve`", and links ADR-008;
  - new section "Events of every task": the flat `TaggedEnvelope`, the `EventCursor` (`<nanos>-<number>-<seq>`, `seq` 0 if absent, `0-0-0`), and the cross-process ordering limit with its workaround.
- `reference/roadmap.md`: new section "0.5 — visibility: history, global activity, full trace, macOS app (done, unreleased)" with 7 ticked items. Nothing else moved.
- `design/persistence.md`:
  - it wrongly said "There is **no cross-process locking**". It now describes `run.lock` and `.index.lock`;
  - the read layer "stores nothing";
  - one follower shared by `vibe serve`;
  - a link to ADR-008.
- `design/agent-runtime.md`: a trace write failure gives a warning and `output_file` null (checked in `runtime.rs:855`), plus links to the Trace page and ADR-008.
- `design/crates.md`: it still said `vibe-pipeline` and `vibe-cli` "declare only `vibe-core` (both are placeholders)".
  - The dependency graph and text now follow the `Cargo.toml` files, including pipeline → agents, workspace.
  - The table rows are real.
  - The `vibe-pipeline` and `vibe-cli` sections are rewritten: run manager, read layer, tests, TUI, server.
- `design/overview.md`: the `vibe-workspace` edge of `vibe-pipeline`, and the diagram labels.
- `design/adr/README.md`: an ADR-008 row.
- `design/adr/007-…md`: a one-line cross-reference to ADR-008 at the end of the trace-store consequence.

### Landing pages
- `README.md`:
  - one more sentence in the pitch;
  - 4 feature rows (history, global activity, full trace, macOS app); no row removed;
  - a "See what was done" section with the three one-liners, plus links to the History and Trace pages;
  - Install covers the macOS app: built from source, DMG with its first release, needs the `vibe` binary;
  - the project layout adds `apps/macos/`, `editors/vscode/` and `evals/`, and the pipeline and CLI descriptions follow;
  - links to the roadmap and the changelog.
- `editors/vscode/README.md`: one sentence saying the web UI shows the activity, the history and the trace, with a link to `vibe serve`. No extension feature is invented.
- `ARCHITECTURE_EN.md` and `ARCHITECTURE.md`, the same changes in both:
  - dependency graph and sentence;
  - the `vibe-agents` row gains `ToolTrace`;
  - the `vibe-pipeline` row gains `RunManager`, `events_log`, `history` and `trace`;
  - the `vibe-cli` row lists every command, `tui`, and `serve` with its shared global stream;
  - a new rule 8 (one seam, events as the contract);
  - a new "Interfaces" table: CLI, TUI, web and API, macOS app, VS Code;
  - the `.vibe/` tree gains the trace store, `memory.jsonl`, `server.token`, `index.json` and `run.lock`.
  - Check: both files are 77 lines, with 17 table lines and 4 fences each.
- `evals/README.md`: **not changed**. `grep` of `tool_returned|events.jsonl|tool-output` in `evals/*.py` finds nothing. `run.py` reads `run.json` and `vibe run --json`, whose shapes only gain optional fields.

### Release notes
- `CHANGELOG.md` `[Unreleased]`: rewritten into Added (12 bullets), Changed (3) and Fixed (5). No version heading.
  - I checked every Fixed entry against `v0.4.0`:
    - `events --follow` on a resumed run: 0.4 set `ended` and never reset it;
    - `run_finished` after an unreported error: 0.4 returned on `?` after `RunStarted`;
    - the run lock;
    - 400 for an ambiguous ref: 0.4's `find` gave 500 on the old routes;
    - panic of JSON output on a closed pipe: 0.4's `print_json` used `println!`.
  - "`?token=` limited to streams" and "symlink → 404" are hardening of new code, so they are not listed.
  - The previous wording "fails before its first phase (workspace, storage)" was inaccurate: 0.4 already ended a failed workspace open with `run_finished`. It is reworded.

## Claims I could not verify, or that depend on others

1. **The macOS app features depend on step 7b, which is in progress.** `git status` shows new `History.swift`, `Trace.swift`, `GlobalEvents.swift` and `TraceViewModel.swift`, but the app README still says History is a placeholder.
   - I only claim what exists at `8d2f5ff`: board, detail, approvals, live activity, menu bar and notifications, "client of `vibe serve`".
   - These places should gain History, Trace and the global Activity once 7b lands:
     - the macOS bullet of `CHANGELOG.md` Added;
     - the README feature row, which does not list them;
     - `docs/src/user/migration.md` "The macOS app";
     - `docs/src/reference/roadmap.md` (last item);
     - `docs/src/user/quickstart.md` step 8;
     - `docs/src/user/history.md` and `trace.md` ("clients … read the same routes").
2. **Upgrade command and version.** `migration.md` shows `cargo install … --tag v0.5.0` and `vibe --version    # vibe 0.5.0`, in the same form as the earlier sections. Neither exists yet: crates are still 0.4.0 and there is no v0.5.0 tag.
3. **The web UI as seen in a browser** was not checked by me. I relied on the step 4 report, which includes a CDP headless smoke.
4. **Not done, since it is outside my list:**
   - PLAN's `gh repo edit` (description and topics), which is outward-facing and left to the lead;
   - ticking `TODOS.md`;
   - the GitHub Pages render. CI publishes it; the local `mdbook build` is fine.
5. **Tension with the ADR index rule.** `adr/README.md` says accepted ADRs are never edited. Step 1 had already added a consequence to ADR-007; I only added the requested cross-reference to ADR-008 in that sentence.

## Corrections after the final review (included in the verification above)

- `persistence.md`: "Readers take no lock" became "readers never hold a lock; the history
  only probes a task's run lock for an instant" (`FileTaskStore::is_running`,
  `store.rs:273`).
- `vibe events <REF> --follow`: the 0.4-visible change is about `--after`. In 0.4 the end
  was detected after the `--after` skip, so an `--after` beyond the `run_finished` seq
  followed forever (checked in `git show v0.4.0:crates/vibe-cli/src/commands/events.rs`).
  `--type` and `--since` are new. CHANGELOG Changed and `migration.md` now say so.
- `vibe tui` `Esc` is not a change: 0.4 quit on `Esc` from its only screen, the board. I
  removed it from CHANGELOG Changed. `migration.md` now says "still quits from the board",
  and notes that `↑` `↓` select calls in the Trace tab.
- `~/.vibe` troubleshooting "Since 0.4.0" is confirmed:
  `git merge-base --is-ancestor a2128ff v0.4.0` gives `in` (a2128ff is the PR #6 merge).
- "`vibe init` has always written `tool-output/`" is confirmed: `init.rs` at `v0.1.0`
  already has it.
- `apps/macos/VibeFactory/README.md` re-read at the end: it still says "*History*: a
  placeholder" and "History and Trace views: they arrive with the server routes". Point 1 of
  the list above stands.

## Verification (last lines, verbatim)

`mdbook build docs`:
```
 INFO Book building has started
 INFO Running the html backend
 INFO HTML book written to `/Users/vincentlauriat/DevApps/Devtools/vibe-factory/docs/book`
```
(no warning; `create-missing = false`, so a missing page would have failed the build)

Link check. I used my own script (`$CLAUDE_JOB_DIR/tmp/linkcheck.py`).
- It covers `docs/src/**/*.md`, `README.md`, `CHANGELOG.md`, both ARCHITECTURE files, `editors/vscode/README.md` and `evals/README.md`.
- It checks every relative `[..](path#anchor)` outside code: the file must exist, and the anchor must match an mdBook-style heading id.
- I validated the script first on a sample with a missing file and a missing anchor: it reported both, and accepted the valid anchors `#changed-files-and-the-approximate-marker` and `#logs-recorded-before-05`.
```
44 files, 237 relative links checked, 0 broken
```

`cargo test -p vibe-core every_event_type_is_documented`:
```
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 52 filtered out; finished in 0.00s
```

`git diff --stat`, my files only. The apps/macos lines in the full output belong to step 7b. Untracked new files: `docs/src/user/history.md`, `docs/src/user/trace.md`, `docs/src/design/adr/008-trace-store-and-read-layer.md`.
```
 ARCHITECTURE.md                                    |  33 +++++--
 ARCHITECTURE_EN.md                                 |  33 +++++--
 CHANGELOG.md                                       |  99 ++++++++++---------
 README.md                                          |  33 ++++++-
 docs/src/SUMMARY.md                                |   3 +
 .../src/design/adr/007-one-seam-many-interfaces.md |   3 +-
 docs/src/design/adr/README.md                      |   1 +
 docs/src/design/agent-runtime.md                   |   5 +-
 docs/src/design/crates.md                          |  72 ++++++++------
 docs/src/design/overview.md                        |   7 +-
 docs/src/design/persistence.md                     |  19 ++--
 docs/src/introduction.md                           |  18 +++-
 docs/src/reference/events.md                       |  35 ++++++-
 docs/src/reference/roadmap.md                      |  18 ++++
 docs/src/user/cli.md                               |  45 +++++++--
 docs/src/user/concepts.md                          |  18 ++++
 docs/src/user/configuration.md                     |  23 ++++-
 docs/src/user/migration.md                         | 108 +++++++++++++++++++++
 docs/src/user/quickstart.md                        |  36 ++++++-
 docs/src/user/troubleshooting.md                   |  88 +++++++++++++++++
 editors/vscode/README.md                           |   5 +-
 21 files changed, 574 insertions(+), 128 deletions(-)
```
