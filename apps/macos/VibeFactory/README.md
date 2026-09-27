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
  - The board, with one entry per task status and badge counts. It is polled every 2 s,
    like the web UI.
  - *Activity*: the selected task's live feed, plus the running and waiting tasks.
  - *History*: a placeholder.
  - *Evaluations*: the `summary.json` tables found under `--evals`.
- **Task detail.**
  - Header with number, title, status, phase and branch.
  - Budget gauges taken from `budget_updated`.
  - Approval banner: Approve takes an optional comment, Reject requires a reason.
  - Tabs:
    - *Overview*: spec, plan with subtask statuses, QA reports, validations.
    - *Activity*: events described like the web UI, plus the streamed `agent_delta` text.
    - *Changes*.
  - The toolbar and the *Task* menu hold New task (⌘N), Run (⌘R), Resume (⇧⌘R),
    Cancel (⌘.), Approve (⌥⌘A) and Reject (⌥⌘J).
- **Menu bar item.** It counts running tasks and pending approvals across all open
  projects.
- **Notifications.** They fire when a gate starts waiting, when a run pauses and when a
  run ends. They are derived from board transitions.
- **Languages.** French and English, following the system language by default. You can
  switch in Settings, where you also set appearance, the path to `vibe`, the evaluations
  directory and notifications.

## Layout

```
project.yml                 xcodegen spec (app target, scheme with the package tests)
VibeFactory/                the app: Models/, Services/, ViewModels/, Views/, Localization/
Packages/VibeAPI/           Swift package: Codable models, VibeClient, SSE parser,
                            EventStream (reconnecting), ServerProcess; XCTest + fixtures
Scripts/make-app-icon.swift placeholder icon generator (from Templates/AppKitTemplate)
Scripts/release.sh          sign + DMG + notarize (release/…); not run yet
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
xcodebuild -scheme VibeFactory -configuration Debug -destination 'platform=macOS' build CODE_SIGNING_ALLOWED=NO
open VibeFactory.xcodeproj                          # run from Xcode (⌘R)
```

The `.github/workflows/macos-app.yml` workflow regenerates the project, runs the package tests and builds the
app. It only does so when `apps/macos/**` or the workflow changes.

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
- **Event stream.** It uses one SSE connection per selected task, sending the token in
  the `Authorization` header. It reconnects with backoff from the last `seq`. If the
  task's run changed in the meantime, it restarts from the beginning of the new run,
  because `seq` restarts at 1 with each run. The parser does not depend on the payload,
  so it will also consume the global `/api/stream`.

## Not there yet

- History and Trace views: they arrive with the server routes (`/api/history`,
  `/api/tasks/{t}/trace`, global `/api/stream`).
- Out of scope for the first version: plan editing, the settings editor and multiple
  servers per window.
- Release: `Scripts/release.sh` is ready but has not been run. The DMG layout and
  Sparkle updates come with the first tagged release.
