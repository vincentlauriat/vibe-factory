# Handoff — Vibe Factory 0.5 "visibility" (local → cloud session)

Written 2026-09-28 by the local Claude Code session before the laptop was shut down.
Talk to Vincent in **French** (full accents); code, commits and docs in **English**.

## Rules of the repository

- Work on `feat/visibility-0.5` (draft **PR #8**). **Do not work on this `wip/...` branch**: it only carries this folder.
- `main` is protected (ruleset "Protect main"): no direct push, PR + 7 green CI checks required.
  **Vincent merges PRs himself** — never merge, never push to `main`, never force-push, never create or move tags.
- Conventional commits, short English messages, present tense. **No `Co-Authored-By: Claude` trailer** (repo rule).
- Anything outward-facing (repo description/topics, releases, tags): **ask Vincent first**.
- Plan of record: `handoff/PLAN.md` (copy of the git-ignored `PLAN.md`). Checklist: `TODOS.md`, section "0.5 — visibility".
  Step reports and reviews: `handoff/reports/`.

## State: all implementation is committed and pushed on `feat/visibility-0.5`

| Step | Content | Commit |
|---|---|---|
| 1 Engine | call ids, full tool outputs (trace store), `committed`/`merged`, `run_finished` totals, task branch | `88a5918` |
| fix | `run.lock` released explicitly (child processes inherited the flock) | `fb422a5` |
| 2 Read layer | incremental reader, all-tasks reader + cursor, `TaskHistory`, `run_trace`, `[pricing]` | `9f6993e` |
| 3 CLI | `vibe history`, `vibe trace`, global `vibe events`, `serve --exit-on-stdin-eof` | `4820d9d` |
| 5 TUI | Activity / History screens, Trace tab | `2e9f5e9`, `945fb06` |
| 4 Server + web UI | `/api/events`, `/api/stream`, `/api/history`, `/api/tasks/{t}/trace`, web views | `8721c3f`, merge `57fe094` |
| 7a macOS app | `VibeAPI` package + SwiftUI app, CI workflow `macos-app.yml` | `8d2f5ff` |
| 6 Docs | all docs + landing pages, `history.md`, `trace.md`, ADR-008, CHANGELOG `[Unreleased]` | `9ca5697` |
| 7b macOS app | global stream, History + Trace views, activity filters, event notifications | `7e311aa` |

Every step had an independent review, its findings fixed, and a separate verification.
Last local checks: Rust 666 tests / 0 failed, fmt + clippy `-D warnings` clean (at `57fe094`, docs-only and
Swift-only commits since); `mdbook build docs` no warning; `swift test` 57 tests (1 smoke skipped) / 0 failed;
`xcodebuild` BUILD SUCCEEDED, 0 warnings in `apps/macos`.

## Next steps, in order

1. `git checkout feat/visibility-0.5 && git pull`. Check CI of PR #8 on `7e311aa` (`gh pr checks 8`). The Swift/Xcode
   part cannot be built in a Linux container — the `macos-app.yml` job is the check for it. Fix anything red
   (root cause, no suppression).
2. Optional follow-ups from the reviews (non-blocking, see `reports/review-step6.md`, `reports/review-step7b.md`):
   the global Activity feed re-filters its 3000-entry buffer on every event (memoize `ActivityViewModel.visible`);
   ADR-007 cross-reference vs "never edited" rule; `config.rs` doc comment `<task>` → `<task dir>`.
   Ask Vincent whether to do them in PR #8 or later.
3. (`TODOS.md` already ticked in `f46d0d3`.) Update PR #8 body (tick 6 and 7b), mark PR #8
   **ready for review** (`gh pr ready 8`), tell Vincent it can be merged once CI is green.
4. Ask Vincent before: `gh repo edit` description + topics (PLAN step 6).
5. Propose release 0.5.0 right after PR #8 is merged (the `deploy-book` CI job publishes the book on every push
   to `main`, so the 0.5 docs go live at merge). Release PR as for 0.4.0 (PR #4): bump 0.4.0 → 0.5.0 in the
   workspace `Cargo.toml`, internal crates, `Cargo.lock`, doc examples, `editors/vscode/package*.json`, the macOS
   app `MARKETING_VERSION`; `[Unreleased]` → `[0.5.0] — <date>`; roadmap/README. Vincent tags after merging.

## Known open points (documented, out of scope for 0.5)

- `is_running` briefly takes the run lock: a `vibe run` started at that exact instant would fail (only when the
  last run has no recorded end).
- A corrupted `task.json` still fails the whole task list (index read).
- Cross-process cursor ordering on `/api/stream` is a documented limit (`docs/src/reference/events.md`).
- The macOS app view models (ProjectSession, TaskDetail/Trace/History VMs) have no unit tests; only the `VibeAPI`
  package is tested.
