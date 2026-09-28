# Independent review of step 7b (macOS app, second half) — outcome

Verdict: no CRITICAL/HIGH, no contract defect. Swift models checked field by field against the Rust
structs; SSE resume/reconnect, token handling (header only, redacted), plain-text rendering, localization
keys all verified. `swift test`: 57 passed, 1 skipped (smoke). Committed as-is in `7e311aa`.

Open, non-blocking:
- MEDIUM `Views/ActivityFeedView.swift:12-14` + `ViewModels/HistoryViewModel.swift:96-100`:
  `ActivityViewModel.visible(feed)` filters the whole 3000-entry buffer on every project-wide event.
  Memoize (invalidate on filter/pause change or growth) or maintain incrementally.
- MEDIUM: no test target for app-level state (ProjectSession, TaskDetail/Trace/History VMs). Extract
  pure functions (dedup, run selection) to test them.
- LOW `Packages/VibeAPI/Sources/VibeAPI/SSE.swift:184-186`: 503 "too many streams" (server MAX_STREAMS=32)
  is retried like any 5xx; could get a distinct message/backoff.
- LOW `Services/ProjectSession.swift:344`: global feed passes `detail: nil`, so subtask ids are not
  resolved to titles (accepted trade-off?).
- LOW `GlobalEvents.swift:203-223`: EventGroup type list mirrors `Event::TYPES` by hand (exhaustive today);
  a fixture-driven exhaustiveness test would catch future event types.
