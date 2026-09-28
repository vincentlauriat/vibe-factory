# Independent review of step 6 (docs) — outcome

Reviewer verified every command, option, route, HTTP status, default, exit code and every
CHANGELOG Fixed/Changed entry against the code, `v0.4.0` and real runs: all exact.

Applied locally and committed in `9ca5697`:
- MAJOR `docs/src/design/crates.md`: dependency diagram drew a false `vibe-pipeline → vibe-providers`
  edge; redrawn from the Cargo.toml files.
- MAJOR `docs/src/user/migration.md`: upgrade block now says "Once 0.5.0 is released:".
- MINOR `troubleshooting.md` + `cli.md` `-C`: "never picks the home directory" → "never climbs up to
  the home directory; running vibe from the home directory itself still uses it".

Left open:
- MAJOR `docs/src/user/history.md:132-134` says the macOS app reads the history routes. True only
  once step 7b is committed (it adds the History view). Commit 7b in the same PR; also update
  `apps/macos/VibeFactory/README.md`, which still calls History "a placeholder".
- Note: `ci.yml` job `deploy-book` publishes the book on every push to `main`. Merging PR #8
  before the 0.5.0 release publishes the 0.5 docs early — do the release PR right after.
- MINOR: `adr/README.md` says accepted ADRs are never edited, yet ADR-007 got a one-line cross
  reference to ADR-008. Either relax the rule for cross references or move the reference.
- NIT: `crates/vibe-core/src/config.rs:150-152` doc comment says `<task>` instead of `<task dir>`.
