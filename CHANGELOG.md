# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Required `pipeline.validation_commands`, executed independently of model verdicts before
  ready/merge. Failures pause for review; results persist in `run.json` and all checks replay
  on resume. Existing configurations keep their previous behavior with an empty list.
- Automatic fixes for failed required commands, with fresh QA and full revalidation.
  `pipeline.max_validation_fix_attempts` (default 2, 0 for manual-only) is retained across
  resumes; policy/configuration failures still require human review.
- Validated automatic integration in a detached worktree, including assisted conflict
  resolution. Reject failing or modified candidates and stale targets before updating the
  base branch. Workspace providers can opt in through `merge_validated` / `MergeValidator`.
- Three dependency-free Rust evaluation cases, an isolated runner and versioned JSON reports.

### Changed
- Prioritize reliability work for v0.2 and clarify that the shell policy is not a sandbox.

## [0.1.0] — 2026-09-26

Initial public release.

### Added
- `vibe-core`: domain model (tasks, specs, plans, QA reports, messages), extension traits
  (providers, tools, workspaces, memory, task store, hooks, plugins), registry, event bus,
  configuration, prompt templates.
- `vibe-providers`: Anthropic Messages API, OpenAI-compatible chat completions, mock
  provider, retry with backoff, HTTP error classification, provider registry.
- `vibe-tools`: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`;
  shell command parser and security policy; path containment.
- `vibe-workspace`: git worktree isolation, merge with conflict reporting, optional
  AI-assisted conflict resolution, in-place mode.
- `vibe-plugins`: JSON-RPC over stdio plugin protocol (MCP-compatible tools), plugin host,
  manifests and discovery, `PluginServer` helper and an example echo plugin.
- `vibe-agents`: agentic loop with parallel non-mutating tools, hooks, context budgeting,
  cancellation, structured output extraction and repair, continuation, built-in prompts and
  agent specs, TOML agent overrides.
- `vibe-pipeline`: file-backed task store, complexity profiles, phase orchestration,
  parallel subtasks with dependencies, QA/fix loop with escalation, resume.
- `vibe-cli`: `vibe init|task|run|status|config|agents|plugins|doctor`.
- Documentation: user guide, design book with ADRs, API docs; CI on three platforms.

[Unreleased]: https://github.com/vincentlauriat/vibe-factory/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/vincentlauriat/vibe-factory/releases/tag/v0.1.0
