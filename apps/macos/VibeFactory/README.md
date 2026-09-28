# Vibe Factory for macOS

A native SwiftUI client of `vibe serve` for macOS 14+. The app shows the task board,
handles approvals and follows live activity. It talks to the engine **only through
the server's HTTP API** (ADR-007: one seam, many interfaces). It never reads `.vibe/`
directly.

## What it does

- **Welcome window.** It lists recent projects. You can open a folder with *Open…* (⌘O)
  or drop one on the window. You can also connect to a server that is already running,
  given its URL and token; the token is stored in the Keychain and keyed by URL.
- **One window per project.** For a folder, the app starts
  `vibe --project <dir> --json serve --bind 127.0.0.1 --port 0`. The child runs without
  a shell and picks a free port. The app reads the URL and token from the child's first
  stdout line and waits for `/api/health` to answer. When the window closes, it sends
  SIGINT, which triggers the server's graceful shutdown: runs are cancelled and stay
  resumable. It sends SIGTERM only if the server outlives its grace period. The same
  happens when the app quits.
- **Sidebar.**
  - The board, with one entry per task status and badge counts. It is reloaded when an
    event changes it, and polled every 10 s because creating a task is not an event
    (every 2 s while the event stream is down), like the web UI.
  - *Activity*: every task's events as they happen, from the project stream.
    - Chips toggle the web UI's groups of event types: agents, tools, phases, git,
      approvals, budget, logs.
    - A picker keeps one task.
    - *Pause* freezes the list and stops the scrolling; the events that arrive meanwhile
      are counted.
    - Clicking a line shows its task in the detail column.
    - The feed keeps the last 3,000 lines; streamed text is not kept there.
  - *History*: what each finished task delivered, with the columns of `vibe history`:
    #, title, status, runs, commits, files, tokens, active time, cost and finished.
    - `+` marks a lower bound: some totals are unknown, or some tokens were not priced.
    - `~` marks an approximate file list, and `-` means there is no pricing.
    - *Show failed and cancelled* adds those tasks.
    - The detail column shows the selected task: its runs, commits, changed files,
      validations, last QA verdict and errors.
      - Each run shows its state, resumes, totals, phases, merge, pending gate and last
        error.
      - Each commit shows its files.
      - Changed files show their status and where the list comes from, with an
        *approximate* badge when it applies.
    - The list reloads each time a run finishes.
  - *Evaluations*: the `summary.json` tables found under `--evals`.
- **Task detail.**
  - Header with number, title, status, phase and branch.
  - Budget gauges taken from `budget_updated`.
  - Approval banner: Approve takes an optional comment, Reject requires a reason.
  - Tabs:
    - *Overview*: spec, plan with subtask statuses, QA reports, validations.
    - *Activity*: events described like the web UI, plus the streamed `agent_delta` text.
      The tab loads the logged events of the last run, then follows the project stream.
    - *Trace*: every tool call of a run.
      - A picker chooses the run when there are several.
      - Each call shows its number, role, subtask, tool, duration and exit code, with
        badges for errors, timeouts and calls that were `paired by order` (logged before
        0.5).
      - A call expands to its arguments as indented JSON and its preview.
      - *Load complete output* fetches the traced output into a scrollable monospaced
        pane. The pane shows the first 200,000 characters, and the window keeps the last
        4 outputs. Outputs are dropped when you pick another run.
      - A call whose output was not traced says so.
      - The footer counts calls and errors and lists the files written.
    - *Changes*.
  - The toolbar and the *Task* menu hold New task (⌘N), Run (⌘R), Resume (⇧⌘R),
    Cancel (⌘.), Approve (⌥⌘A) and Reject (⌥⌘J).
- **Menu bar item.** It counts running tasks and pending approvals across all open
  projects.
- **Notifications.** They fire when a gate starts waiting, when a run pauses and when a
  run ends, for every task of the project. They come from the project stream, one per stop
  of a run: a gate logs `approval_requested`, `paused` and then `run_finished`, and only the
  first of them notifies. The events loaded when the window opens never notify.
- **Languages.** French and English, following the system language by default. You can
  switch in Settings, where you also set appearance, the path to `vibe`, the evaluations
  directory and notifications.

## Layout

```
project.yml                 xcodegen spec (app target, scheme with the package tests)
VibeFactory/                the app: Models/, Services/, ViewModels/, Views/, Localization/
Packages/VibeAPI/           Swift package: Codable models (tasks, events, TaggedEnvelope,
                            EventCursor, TaskHistory, RunTrace), VibeClient, SSE parser,
                            GlobalStream and EventStream (reconnecting, longer backoff on
                            503), ServerProcess, the Activity filter, History row text,
                            notices, the view models' pure logic (FeedLogic.swift);
                            XCTest + fixtures (event-types.json written by a cargo test)
Scripts/make-app-icon.swift placeholder icon generator (from Templates/AppKitTemplate)
Scripts/release.sh          sign + DMG + notarize + Sparkle signature, writes appcast.xml
appcast.xml                 Sparkle update feed (SUFeedURL reads it from main)
```

## Requirements

- Xcode 15 or later (Swift 5.9, macOS 14 SDK).
- [xcodegen](https://github.com/yonaskolb/XcodeGen): `brew install xcodegen`.
- `vibe` **≥ 0.5** installed (`cargo install --path crates/vibe-cli`): the app starts
  `vibe serve --exit-on-stdin-eof`, which 0.4 does not know (the smoke test fails
  against 0.4 until `vibe` is reinstalled). The app looks for it in this order: the path
  set in Settings, `~/.cargo/bin/vibe`, then `PATH`.

## Build, run, test

```sh
cd apps/macos/VibeFactory
xcodegen generate                                   # VibeFactory.xcodeproj is generated, not versioned
swift test --package-path Packages/VibeAPI          # package tests
VIBE_SMOKE=1 swift test --package-path Packages/VibeAPI --filter testSmoke   # real vibe serve on this repo
VIBE_SMOKE=1 VIBE_EXECUTABLE=../../../target/debug/vibe \
  swift test --package-path Packages/VibeAPI --filter testSmoke                # …with the checkout's build
xcodebuild -scheme VibeFactory -configuration Debug -destination 'platform=macOS' build CODE_SIGNING_ALLOWED=NO
open VibeFactory.xcodeproj                          # run from Xcode (⌘R)
```

The `.github/workflows/macos-app.yml` workflow regenerates the project, runs the package tests and builds the
app. It only does so when `apps/macos/**` or the workflow changes.

## Release

`./Scripts/release.sh <version>` (from this folder, on the merged `main`) builds Release, signs with the
Developer ID and Hardened Runtime (Sparkle's nested binaries first), makes the DMG in `release/`,
notarizes and staples it, then EdDSA-signs it for Sparkle and rewrites `appcast.xml`. Then:

1. `gh release upload v<version> release/VibeFactory-<version>.dmg` (the appcast's enclosure points there);
2. commit `appcast.xml` to `main`: installed apps read `SUFeedURL` from
   `raw.githubusercontent.com/…/main/apps/macos/VibeFactory/appcast.xml`.

The Sparkle private key lives in the login keychain under account `VibeFactory`; its public half is
`SUPublicEDKey` in `project.yml`. Never regenerate it: installed apps would refuse every later update.

## Decisions

- **No App Sandbox.** The app starts `vibe serve` as a child process, and the agents it
  drives run git and commands in any project folder the user opens. A sandboxed app
  would need a signed helper and security-scoped bookmarks for each folder. Recent
  projects are therefore stored as plain paths. Hardened Runtime stays on for
  notarisation.
- **Child environment.** A Finder-launched app gets a minimal `PATH`, so the app adds
  `~/.cargo/bin`, `/opt/homebrew/bin` and `/usr/local/bin`. Variables exported only in
  shell startup files, such as the provider API keys named by `api_key_env`, are **not**
  seen: enter them in Settings → *Server environment variables* (values kept in the
  Keychain, passed to each `vibe serve` the app starts), or connect to a `vibe serve`
  started from a terminal.
- **No orphan servers.** The child's stdin is a pipe the app keeps open and never writes
  to; with `--exit-on-stdin-eof` the server shuts down gracefully when the app dies, even
  by a crash or SIGKILL. A child that dies on its own turns the window to an error with
  Retry.
- **One event stream per project.** When it connects, the window:
  1. reads the last 500 events with `GET /api/events`;
  2. opens `GET /api/stream?after=<X-Vibe-Cursor of that answer>`.

  If nothing has been logged yet, there is no cursor. The stream then starts from the
  time taken just before the first call, so nothing logged in between is lost.
  - The token goes in the `Authorization` header, not in `?token=`.
  - After a cut, the stream reconnects with backoff from the id of the last logged event,
    sent as `Last-Event-ID` and as `after`.
  - An id is an event cursor, `<nanoseconds>-<task number>-<seq>`. The app keeps it as
    integers exact to the nanosecond and never rebuilds one from a date.
  - Ids are not always increasing across tasks, so events are not dropped by comparing
    them.
  - Streamed text (`agent_delta`) has no id and does not move the resume point.
- **The stream feeds everything.** Board reloads, the Activity feed, the selected task,
  History reloads and notifications all come from it.
  - The selected task first subscribes, then loads the logged events of its last run.
    Live events that arrive meanwhile are applied after them.
  - A `(run, seq)` check drops the overlap.
  - The per-task `EventStream` stays in the package, but the app no longer opens it.

## Not there yet

- A manual session against `vibe serve` on a real project has not been done yet: the views
  are built, the data layer and the view models' pure logic are tested, but nobody has
  clicked through them (the view models themselves, `@MainActor` and bound to a live
  server, have no tests).
- Out of scope for the first version: plan editing, the settings editor and multiple
  servers per window.
- The DMG has an `/Applications` alias but no Finder background layout.
