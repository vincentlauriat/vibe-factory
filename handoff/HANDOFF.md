# Handoff — Vibe Factory 0.5 "visibility" (cloud session → local Claude Code)

Written 2026-09-28 by the cloud session (https://claude.ai/code/session_014nmyM24rNX2fwSVEeH8Sgd) before
Vincent went back to his Mac. Talk to Vincent in **French** (full accents); code, commits and docs in **English**.

## Rules of the repository (unchanged)

- `main` is protected: no direct push, PR + green CI required. **Vincent merges PRs himself** — never merge,
  never push to `main`, never force-push, never create or move tags.
- Conventional commits, short English messages, present tense. **No `Co-Authored-By: Claude` trailer**.
- Anything outward-facing (repo description/topics, releases, tags): **ask Vincent first**.
- Plan of record: `handoff/PLAN.md`. Checklist: `TODOS.md`. Step reports and reviews: `handoff/reports/`.
- This `wip/visibility-0.5-handoff` branch only carries this folder; do not develop on it.

## What the cloud session did (2026-09-28, 06:00–08:00 UTC)

On `feat/visibility-0.5` (PR #8, now **ready for review**, `mergeable_state: clean`, head `beabae8`):

| Commit | Content |
|---|---|
| `2213ad6` | test(cli): `wait_for` helper gated to `#[cfg(unix)]` like its two callers. The Windows CI job failed on dead code under `-D warnings`; every other check was green. |
| `5c6c629` | docs: `adr/README.md` now allows adding a cross reference to a later ADR in an accepted ADR (the ADR-007 → ADR-008 link stays); `config.rs` doc comment says `<task dir>`. |
| `beabae8` | perf(macos): `ActivityViewModel.visible(feed)` keeps its result (`@ObservationIgnored` cache keyed by filter + pausedAt) and only filters the entries the feed added since; drops trimmed heads by binary search on ids; starts over when the filter or the pause changes. |

CI on `beabae8`: all 9 checks green (ubuntu/macos/windows, MSRV 1.88, docs, evals, VS Code, macOS app
build + `swift test`). PR #8 body updated (steps 6 and 7 ticked, CI fix and follow-ups listed).

Local checks on the release tree: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features`
with `-D warnings`, `cargo test --workspace` (0 failures), `vibe --version` → `vibe 0.5.0`.

On `release/0.5.0` (branched from `feat/visibility-0.5`, one commit `1009cd2` "chore(release): 0.5.0"), **PR #9**
open against `main`: workspace + internal crate versions 0.5.0, `Cargo.lock`, `crates/vibe-plugins/src/lib.rs`
example, `docs/src/design/plugin-protocol.md`, `installation.md` (tag + `vibe 0.5.0`), `migration.md` ("Once
0.5.0 is released:" removed), `roadmap.md` and `TODOS.md` say released, `editors/vscode/package*.json` 0.5.0,
macOS `project.yml` `MARKETING_VERSION` 0.1.0 → 0.5.0, CHANGELOG `[Unreleased]` → `[0.5.0] — 2026-09-28` with
compare links (`[Unreleased]` kept empty above it).

Repository description and topics: Vincent applied them himself with `gh repo edit` (the cloud proxy refuses
repository-settings writes). Done, nothing left there.

## State to check first

At 07:50 UTC Vincent said PR #8 was merged, but GitHub still showed it **open** and `main` still at `45f33cb`
(pre-0.5). Verify with `gh pr view 8` / `git fetch origin main`. The merge is Vincent's; if it is still open,
tell him plainly.

## Next steps, in order

1. `gh pr checks 9` — CI of the release PR on `1009cd2` (it also carries PR #8's commits until PR #8 is merged;
   the diff then shrinks to the release commit, no rebase needed since the repo merges with merge commits).
   Fix anything red (root cause, no suppression). If Vincent merges PR #8 with a squash instead, rebase
   `release/0.5.0` onto `main` (that is a branch the cloud session created, a force-with-lease push is fine
   there **only** with Vincent's ok).
2. Once PR #8 is merged: confirm PR #9's diff is the single release commit and CI is green, tell Vincent he
   can merge it. Vincent then tags `v0.5.0` on the merge commit → `release.yml` builds the binaries;
   `deploy-book` publishes the 0.5 book on the push to `main`.
3. After the tag: check the release workflow, the GitHub release notes (CHANGELOG `[0.5.0]` section), the
   book front page. The macOS app is "built from source" for 0.5 (README + roadmap say so); a signed DMG via
   `apps/macos/VibeFactory/Scripts/release.sh` is a separate decision for Vincent.
4. Optional, later (out of scope for 0.5, listed in PR #8 "Left for later"): unit tests for the macOS view
   models (extract pure functions: dedup, run selection, the new `visible` cache); SSE 503 "too many streams"
   distinct backoff; global feed `detail: nil` so subtask ids are not resolved to titles; fixture-driven
   exhaustiveness test for `EventGroup` vs `Event::TYPES`.

## Known open points (documented, out of scope for 0.5)

- `is_running` briefly takes the run lock: a `vibe run` started at that exact instant would fail (only when the
  last run has no recorded end).
- A corrupted `task.json` still fails the whole task list (index read).
- Cross-process cursor ordering on `/api/stream` is a documented limit (`docs/src/reference/events.md`).
- The macOS app view models have no unit tests; only the `VibeAPI` package is tested.
