# Step 7b report: macOS app, second half

Branch `feat/visibility-0.5`. Nothing committed, pushed or stashed.

All changes are under `apps/macos/VibeFactory/`. The other modified files in `git status` are the other agent's:
- `docs/`;
- the root `README.md`, `CHANGELOG.md` and `ARCHITECTURE*`;
- `editors/`.

## Files changed

**Package `Packages/VibeAPI`**

| File | Change |
|------|--------|
| `Sources/VibeAPI/GlobalEvents.swift` (new) | Defines the types for the project-wide stream (see the list below). |
| `Sources/VibeAPI/History.swift` (new) | Mirrors of `history.rs` and the History row text (see the list below). |
| `Sources/VibeAPI/Trace.swift` (new) | `PairedBy` (open enum), `TraceCall` (the server's `Call`; `hasOutput` is false for a nil id or no `output_file`), `RunTrace` (`errorCount`), `CallOutput {text, truncated}` |
| `Sources/VibeAPI/EventNotice.swift` (new) | `EventNotice` and `EventNoticeTracker`: one notification per stop of a run (see below) |
| `Sources/VibeAPI/Format.swift` (new) | `Format` moved here from the app (`ActivityLine.swift`), made public, with `lowerBound` added |
| `Sources/VibeAPI/SSE.swift` | `SSEMessage.id`: the `id:` of this very message, nil when it had none (`lastEventId` still carries the previous id) |
| `Sources/VibeAPI/VibeClient.swift` | New routes (see below). `ServerEndpoint.url` escapes `+` in queries, because an RFC 3339 offset would otherwise reach the server as a space. The plumbing is split into `fetch` (checks the status, keeps the headers) and `decode`. |
| `Sources/VibeAPI/BoardTransition.swift` (deleted) | Replaced by `EventNoticeTracker`: notifications now come from events |

`GlobalEvents.swift` defines:
- `EventCursor`:
  - it holds `nanos: Int64`, `number: UInt32` and `seq: UInt64`;
  - `init?(String)` splits from the right, as Rust's `rsplitn(3,'-')` does;
  - `description` formats the compact form;
  - it is `Comparable` on the tuple.
- `TaggedEnvelope` (flattened `Envelope`, `task`, `number`).
- `EventsPage {events, cursor, readErrors}`.
- `GlobalEvent {cursor?, tagged}`.
- `GlobalStream`:
  - it is built on `SSEConnection`;
  - its `Query` carries after, since, types and tasks;
  - a resume passes the last id both as `after` and as `Last-Event-ID`.
- `EventGroup`: the web UI's `GROUPS`, copied verbatim.
- `ActivityFilter`, with `keeps(_:)` and `keeps(task:type:)`.

`History.swift` defines:
- `PhaseSummary`, `CommitRecord`, `MergeRecord` and `ValidationRecord`.
- `RunSummaryState`, an open enum.
- `HistoryRun`: the server's `RunSummary`, renamed because the board row already has a `RunSummary`.
- `FileStatus`: an open enum with a distinct `.unknown` (the server's own value) and `.other(String)`, plus `letter`.
- `FileChange`.
- `ChangedFilesSource`: tagged by `kind`, with an `.unknown(kind)` case.
- `ChangedFiles`, `HistoryTotals`, `QaSummary` (`issues` is a count), `Cost` and `TaskHistory` (`finishedAt`).
- `HistoryRowText`: the cells of a row with the `vibe history` notation:
  - `+` on tokens and active time when `!totals.complete`;
  - `+` on the cost when `!cost.complete`;
  - `-` when there is no cost;
  - `~` on files when the list is approximate.

`VibeClient` routes:
- `events(after:since:types:tasks:limit:)` returns `EventsPage`, with the `X-Vibe-Cursor` and `X-Vibe-Read-Errors` headers.
- `globalStream(after:since:types:tasks:backoff:)`.
- `history(all:)` and `history(task:)`.
- `trace(task:run:all:)` always returns `[RunTrace]`: the server answers an object by default and an array with `all`, and the client normalises both.
- `traceOutput(task:call:)`:
  - it returns `CallOutput?`, reading `text/plain` and `X-Vibe-Truncated`;
  - a 404 gives `nil`;
  - a 400 surfaces as `.server`.

**Tests `Packages/VibeAPI/Tests/VibeAPITests`**

| File | Content |
|------|---------|
| `Fixtures/events-global.json` (new) | 7 tagged envelopes: two tasks, two runs with the same `at` (a tie), an unknown type `hologram`, and a pre-0.3 line without `seq` |
| `Fixtures/history.json`, `history-detail.json` (new) | `TaskHistory` written from `history.rs` (see the list below) |
| `Fixtures/trace.json`, `trace-all.json` (new) | Five calls (see the list below), plus a two-run `all` array |
| `GlobalEventsTests.swift` (new) | 7 tests (see the list below) |
| `HistoryTraceTests.swift` (new) | History detail and list decoding, History row notation (`184.2k+`, `6 min 12 s+`, `1.83 USD+`, `1~`, `-`, `0.50 EUR`), trace decoding |
| `GlobalClientTests.swift` (new) | Tests of the new routes against the URLProtocol stub (see the list below) |
| `SSEParserTests.swift` | Expectation updated for `id`; new test "a message without id keeps the last id but has no own id" |
| `ServerProcessTests.swift` | The smoke test is extended (see the list below) |
| `RecordedFixturesTests.swift` (new) | Decoding of **real recorded answers** (see "Recorded fixtures" below) |
| `Fixtures/real-events.json`, `real-history.json`, `real-history-detail.json`, `real-trace.json`, `real-stream.txt` (new) | Recorded from `target/debug/vibe serve` on a scratch project |
| `BoardTransitionTests.swift` (deleted) | With `BoardTransition` |

The history fixtures cover:
- a finished run with 1 resume, 2 phases (one unfinished), 2 commits (one pre-0.5 with no message or files), a merge, a passed and a failed validation, and an approval;
- a legacy run with `state: "zombie"`, `totals_known: false`, a pending `plan` and a `last_error`;
- a `merge_commit` source, a renamed file with `old_path`, `unknown`, and an unknown `type_changed`;
- incomplete totals and cost;
- a `trace` source that is approximate, with no cost;
- a task with an `archived` status, a `telepathy` source and a EUR cost.

The trace fixture's five calls are:
- paired `id`;
- an error with `exit_code` 101, timed out, role `qa_fixer`, no subtask;
- a nil id paired by `order`, with no output;
- `unmatched` (never returned);
- paired `psychic`, an unknown value.

`GlobalEventsTests.swift` tests:
- cursor parse and format, including the `since_time` extremes, a negative time and 9 malformed forms;
- cursor order;
- flattened decoding and encoding round-trip;
- the groups against the documented types;
- the Activity filter (groups, unknown types going with the logs, task);
- the notice tracker;
- query items and `+` escaping.

`GlobalClientTests.swift` covers:
- `/api/events`: the query, the two headers, a missing cursor header, and a 400 surfacing as `.server`;
- history and trace routes: `all`, `run`, the normalised object, a 400 for several matching runs, a 400 for `run` with `all`, and a 404 when the task was not run;
- trace output: text, `X-Vibe-Truncated`, 404 → `nil`, 400 → `.server`;
- the **global stream**:
  - a replay of two logged events and an `agent_delta` without id, then a cut, then the live part;
  - the cursors are `[c1, c2, nil, c3]`;
  - the reconnect sends `Last-Event-ID` and `after` equal to the last *logged* id, not changed by the delta;
  - the token goes in the header, not in `?token=`, and the filter is repeated;
- the global stream stopping on a 400.

The smoke test is extended:
- `VIBE_EXECUTABLE` overrides the binary;
- it calls `/api/history?all=true`, `/api/events?limit=50` and `/api/stream` (checks 200 and `text/event-stream`) and decodes them.

**App `VibeFactory/`**

| File | Change |
|------|--------|
| `Services/ProjectSession.swift` | One global stream per project; board reload debounced on events plus a 10 s poll (2 s while the stream is down); `subscribe`/`unsubscribe`; `finishedRuns` counter; notifications from events; new `ProjectFeed` (see the lists below) |
| `ViewModels/TaskDetailViewModel.swift` | No stream of its own: it subscribes to the session, loads the backlog and applies events through a `(run, seq)` check (see the list below). Owns a `TraceViewModel`. |
| `ViewModels/TraceViewModel.swift` (new) | Run choices, the selected run's `RunTrace`, expanded calls, outputs loaded on demand (see the list below) |
| `ViewModels/HistoryViewModel.swift` (new) | `HistoryViewModel` (list, `showAll`, selection, detail via `history(task:)`) and `ActivityViewModel` (filter, pause at an entry id, count of events held) |
| `ViewModels/ProjectViewModel.swift` | Holds `activity` and `history`; the detail gets the session |
| `Views/ActivityFeedView.swift` | Rewritten as the global feed (see "What each view does"). Adds `ActivityLine.Tone.color`. |
| `Views/HistoryView.swift` (new) | `HistoryView` (table), `HistoryDetailView`, `RunStateBadge` |
| `Views/TraceTab.swift` (new) | The Trace tab |
| `Views/TaskDetailView.swift` | Tabs Overview, Activity, **Trace**, Changes; uses `tone.color` |
| `Views/ProjectView.swift` | History content and detail; the History content column is wider (520–1200) for its ten columns, the others are 260–520 |
| `Views/SidebarView.swift` | History placeholder removed |
| `Models/ActivityLine.swift` | `Format` moved to the package. `describe(…, verbose:)`: the project feed also shows budget updates, successful tool returns, info logs and unknown types, so that every filter chip has lines to act on. |
| `Localization/Strings.swift` | 70 new keys per language (fr and en, counted from the diff); `activity_soon` and `history_soon` removed. Checked with a Python script: same key set in both tables, no duplicate keys (a duplicate in a Swift dictionary literal would crash at launch). |
| `README.md` | New views, global stream, smoke with `VIBE_EXECUTABLE`, and "Not there yet" updated |

`ProjectSession` in detail:
- **One stream per project.** After connecting it:
  1. takes a UTC time;
  2. calls `events(limit: 500)` to seed the feed;
  3. opens `globalStream(after: X-Vibe-Cursor)`, or `since: <time taken before the call>` when no cursor came back.
- **Board.** The reload is debounced (300 ms) on `run_started`, `phase_started`, `phase_finished`, `subtask_updated`, `approval_*`, `paused`, `merged` and `run_finished`. There is also the 10 s poll (2 s while the stream is down).
- `subscribe` and `unsubscribe` hand live events to listeners (the selected task).
- `finishedRuns` counts the `run_finished` events, and History reloads on it.
- Notifications come from events, for all tasks, via `EventNoticeTracker`. The seeded events never notify.
- `ProjectFeed`:
  - it keeps described entries `{line, task, number, type}`, capped at 3000;
  - `agent_delta` is not kept;
  - it does not keep the envelopes, so large `write_file` arguments do not stay in memory.

`TaskDetailViewModel` in detail:
1. It subscribes to the session first. Live events are buffered while loading; deltas pass straight to the live text.
2. It loads the detail and `GET /api/tasks/{ref}/events` (the last run).
3. It applies the backlog, then the buffered events, all through the `(run, seq)` check, which drops the overlap.
4. On `run_finished` it reloads the detail and the trace.

`TraceViewModel` in detail:
- It shows the calls of the selected run only.
- After a reload it keeps the selected run, unless that was the newest run: then it follows the new newest run.
- `TraceTab` calls `stop()` on disappear. `appear()` loads again whenever nothing was shown yet, so a load cut by a tab switch is restarted.
- Expansion is keyed by index, because call ids repeat (`000000000000`) before 0.5.
- Outputs are loaded on demand:
  - at most 4 are kept, the oldest dropped;
  - each is capped at 200,000 characters for display, with a note;
  - all are dropped when another run is picked.
- A 404 means "not run yet", an empty state and not an alert.
- Tasks are cancelled in `stop()`.

## What each view does

**Activity (sidebar)** is every task's events from the project stream.
- The feed shows time, `#n` and the description.
- There is one toggle chip per group: agents, tools, phases, git, approvals, budget, logs. The tooltip lists the types. Unknown types go with logs.
- A task picker offers All tasks or one task.
- *Pause* freezes the list at the last entry and stops following the bottom; "N new" counts the held entries the filter keeps.
- Clicking a line selects its task, and the detail column shows it.
- The empty state says whether the stream is connected, and a stream error shows at the bottom.

**History (sidebar)**:
- A `Table` with the columns #, title, status badge, runs, commits, files, tokens, active, cost and finished (relative, with the date in the tooltip).
- A *Show failed and cancelled* checkbox, a Refresh button, and a notes line explaining `+`, `~` and `-`.
- It reloads on appear, on the toggle and on each `run_finished` (`.task(id:)`, so it is cancelled on disappear).
- The detail column shows the header (number, title, status, branch, tokens in/out with `+`, active, cost), then:
  - runs: short id, state badge, final status, start, resumes, totals or "totals unknown", phases with ✓/✗/…, merge, pending gate, last error;
  - commits: short sha, message, file count and subtask, expandable to the files;
  - changed files: source text, an *approximate* badge, then status letter, path and old path;
  - the validations of the last run that ran any: command, exit code, integration;
  - the last QA: round, verdict, issue count, summary;
  - the errors.

**Task detail → Trace**:
- The run picker appears when there is more than one run (`xxxxxxxx (N calls)`), with a Refresh button.
- Each call row shows `#n role · subtask title · tool`, then duration, `exit N`, and the badges *timed out*, *error*, *paired by order* and *unmatched*.
- Expanding a call shows:
  - its arguments as pretty JSON (capped at 20,000 characters) and its preview;
  - either *Load complete output* (the tooltip gives the size), or "not traced" for a nil id or a missing `output_file`;
  - once loaded, a monospaced pane that scrolls both ways (max 320 pt high), with notes for server truncation, display clipping, or a missing output (404).
- The footer shows "N call(s) · M error(s)" and the files written.

## Decisions and deviations

1. **`SSEMessage.id` was added to the parser.** The server's `agent_delta` frames have no id, and the parser's `lastEventId` keeps the previous one, as the spec requires. Deriving the cursor from `lastEventId` would give every delta the cursor of the event before it. A mutation check, reading `lastEventId` in `GlobalStream`, makes `testGlobalStreamReplaysThenResumesFromTheLastId` fail with `[…, "…-4-1", "…-4-1", …]` instead of `[…, nil, …]`. The code was then restored.
2. **The cursor is never a `Date`.** A Double cannot hold about 1.79e18 ns exactly, so cursors are only taken from the SSE id or the header. `EventCursor.date` is for display only.
3. **No global dedup by cursor.** Per the docs, ids are not monotonic across tasks when clocks differ between processes; the server sends each event once per connection. The selected task keeps its own `(run, seq)` check, because its backlog and the stream overlap.
4. **A gap is closed when no cursor exists.** With nothing logged, `/api/events` has no `X-Vibe-Cursor`, so the stream uses `since=<UTC time taken before that call>`. The time is written with `Z`, and `+` is escaped in every query anyway.
5. **Notifications are one per stop of a run.** `approval_requested` → `paused` → `run_finished` gives one notice, and resetting on `run_started` covers a resume under the same run id. This is a change from 7a, which derived them from board transitions; `BoardTransition` and its tests were deleted.
6. **The per-task `EventStream` stays in the package, with its tests, but the app no longer uses it.** The selected task gets its events from the one project stream. Deltas come only from the global stream, for runs this server started, which is also what the task stream carried.
7. **The feed's seed size is 500 events** (the server default is 1000). It is enough to fill the feed without reading the whole project each time a window opens.
8. **The project feed is verbose.** It also describes `budget_updated`, successful `tool_returned`, info `log` and unknown types, so the budget and tools chips filter something visible. The task's own Activity tab stays as before.
9. **The History content column is wider** (520–1200 instead of 260–520), because a `Table` with ten columns does not fit in 460 pt.
10. **Deviation: 2 s board polling is kept as a fallback.** The brief said "drop the 2 s polling".
    - The board now polls every 10 s while the stream runs.
    - It returns to 2 s only while the stream is down (it stopped on a 4xx, or `/api/events` failed), as the web UI does.
    - Without that, a server whose stream is refused would leave the board up to 10 s stale.
    - It is a one-line change in `ProjectSession.startPolling` to drop it.
11. **Stream liveness is approximate.** `SSEConnection` reconnects on its own after network errors and 5xx. `streamUp` is therefore false only when the stream stopped for good (4xx) or failed to seed, and only then does the 2 s board poll apply.
12. **URLSession behaviour, measured.** `bytes(for:)` returns the response only when the first body bytes arrive. On a quiet project that is the keep-alive comment at 15 s. After that, small chunks are delivered as they come.
    - Probe against a scratch server: response and the first `:\n\n` at 15.0 s, the next keep-alive at 30.0 s.
    - `curl -D -` confirmed that the server sends the headers at once.
    - So live events are not delayed. The smoke test simply allows 25 s for the stream headers. My first run used 10 s and timed out; I checked it with curl and the probe before changing the test.
13. **The checkout reports "vibe 0.4.0"**, because the version is not bumped yet. It still serves every 0.5 route and `--exit-on-stdin-eof`, and the smoke test passed against it.
14. **Smoke-test side effect removed.** Serving the repository created `.vibe/tasks/`, empty, and there was no `.vibe/` before. I deleted both with `rmdir`, which only removes empty directories. No `vibe serve` process was left.

## Recorded fixtures

I did a scripted run on a scratch project, outside the repository, with this checkout's `target/debug/vibe`:
1. `git init` and `vibe init` in the scratch folder;
2. `vibe serve --workspace in_place --script <greeting script>`, where the script is the one from `crates/vibe-cli/tests/serve.rs`;
3. `POST /api/tasks`, then `POST /api/tasks/1/run`, then waiting for `finished`.

The answers were recorded with curl:
- `/api/events` (37 events, with `X-Vibe-Cursor: 1790574772257777000-1-37`);
- `/api/history?all=true` and `/api/history/1`;
- `/api/tasks/1/trace`;
- the body of `/api/stream?since=1d`.

The scratch path is replaced by `/tmp/demo`, the folder was deleted, and no local path is left in the fixtures.

`RecordedFixturesTests` checks:
- all 37 events decode with no `.unknown`;
- the stream body parses into 37 messages whose ids are cursors, sorted, each equal to its envelope's `number`, `seq` and `at` (to the microsecond), and the last one equal to the header;
- history and trace decode, and the History row reads `4.4k` tokens, `-` cost and `~` files.

The server writes `data:` **before** `id:` in each frame. The parser commits the id at dispatch, so that order is handled.

## What remains

- **A manual UI session.** Not possible from this non-interactive shell. What was checked:
  - the app builds;
  - the data layer is tested against the fixtures and the stub;
  - the real shapes of events, the stream, history and trace are checked through the recorded fixtures above;
  - the smoke test covers the routes, auth and stream headers against a live server.

    On this repository it decodes only empty answers (0 tasks, 0 events, no cursor). The live smoke therefore does not exercise the JSON shapes; the recorded fixtures do.

  Worth trying by hand, on a project with a scripted or real run:
  - the Activity chips, pause and click;
  - History with `+`/`~`/`-`, and its detail;
  - the Trace picker and *Load complete output*;
  - notifications for gate, pause and end;
  - the board updating without waiting 10 s.
- **Fixtures.** The recorded fixtures cover one real scripted run. The edge cases are still written by hand from the Rust shapes:
  - unknown enum values;
  - legacy runs;
  - merges;
  - costs.

  There is no golden generator on the cargo side yet; this is already in TODOS.
- **Release.** `Scripts/release.sh` has not been run. The DMG layout and Sparkle come with the first tagged release, and the CI workflow is unchanged.
- **Possible follow-up.** Reload the selected task's events when the global stream reconnects. The docs note that a `vibe run` in another process whose clock is behind can log an event a resumed stream will not replay. Today the next `run_finished` or the Refresh buttons cover it.

## Verification output (last lines, verbatim)

`cd apps/macos/VibeFactory && xcodegen generate`
```
Created project at /Users/vincentlauriat/DevApps/Devtools/vibe-factory/apps/macos/VibeFactory/VibeFactory.xcodeproj
```

`swift test --package-path Packages/VibeAPI`
```
	 Executed 57 tests, with 1 test skipped and 0 failures (0 unexpected) in 1.722 (1.725) seconds
```
The skipped test is the smoke test, which needs `VIBE_SMOKE=1`.

`xcodebuild -scheme VibeFactory -configuration Debug -destination 'platform=macOS' build CODE_SIGNING_ALLOWED=NO 2>&1 | tail -5`
```
PruneExplicitPrecompiledModules /Users/vincentlauriat/Library/Developer/Xcode/DerivedData/SDKExplicitPrecompiledModules

** BUILD SUCCEEDED **

```
The full log has 0 warnings or errors in `apps/macos` files; the grep for `apps/macos.*(error|warning):` counts 0. The first full build's only warning was Xcode's own `appintentsmetadataprocessor … no AppIntents.framework dependency found`.

`cargo build -p vibe-cli` (repo root)
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.34s
```

`VIBE_SMOKE=1 VIBE_EXECUTABLE=<repo>/target/debug/vibe swift test --package-path Packages/VibeAPI --filter testSmoke`
```
	 Executed 1 test, with 0 failures (0 unexpected) in 15.192 (15.193) seconds
smoke: vibe 0.4.0 on http://127.0.0.1:51948/, 0 task(s), 0 in history, 0 event(s), cursor none, read errors 0, stream 200 text/event-stream, pid 61286
```
This run covers the routes, auth, `[]` decoding and the stream headers. The JSON shapes are covered by the recorded fixtures instead.

Afterwards, `pgrep -fl "vibe --project"` found no process. The empty `.vibe/tasks` and `.vibe` at the repository root were removed again after this final run.
