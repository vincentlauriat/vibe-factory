//! # vibe-pipeline
//!
//! Task store and multi-agent pipeline orchestration for **Vibe Factory**.
//!
//! This crate depends only on `vibe-core` and `vibe-agents`: model
//! providers, tools, workspaces and commits are injected through
//! [`PipelineDeps`] (and the plugins of a [`vibe_core::Registry`]).
//!
//! ## Phases
//!
//! ```text
//!            ┌────────┐   ┌──────┐   ┌──────┐   ┌───────┐   ┌────┐   ┌───────┐
//!  task ───▶ │ assess │──▶│ spec │──▶│ plan │──▶│ build │──▶│ qa │──▶│ merge │──▶ Ready / Done
//!            └────────┘   └──────┘   └──────┘   └───────┘   └────┘   └───────┘
//!                          (skipped              (parallel    │ ▲
//!                           for trivial)          subtasks)   ▼ │ changes requested
//!                                                           ┌─────┐
//!                                                           │ fix │
//!                                                           └─────┘
//! ```
//!
//! For every phase the pipeline: runs the `before_phase` hooks (an abort
//! ends the run as **Cancelled**), publishes `PhaseStarted`, resolves the
//! phase model (`config.model_for(phase)`, or the agent's fixed model),
//! builds an agent runner whose tools are confined to the workspace, runs
//! the phase, publishes `PhaseFinished`, runs the `after_phase` hooks and
//! persists `run.json`. The task status follows along: **Planning** during
//! assess/spec/plan, **Building** during build, **Review** during
//! QA/fix/merge, then **Ready**, **Done**, **Failed** or **Cancelled**.
//!
//! ## Profiles
//!
//! | Complexity | Phases | Spec steps |
//! |------------|--------|------------|
//! | trivial    | assess, plan, build, qa, merge | none (the planner works from the task description) |
//! | simple     | assess, spec, plan, build, qa, fix, merge | gatherer only |
//! | standard   | assess, spec, plan, build, qa, fix, merge | gatherer, writer |
//! | complex    | assess, spec, plan, build, qa, fix, merge | gatherer, researcher, writer, critic |
//!
//! The complexity comes from `--complexity` ([`RunOptions::complexity_override`]),
//! then a complexity already stored on the task, then
//! [`heuristic_complexity`] (≤ 30 words and a "fix typo" / "bump version" /
//! "rename" / "change label" / "remove unused" pattern), then the
//! `complexity_assessor` agent; it falls back to **standard** if the agent
//! fails. The assessor's `needs_research` / `needs_critique` flags switch on
//! the research and critique steps of a full spec.
//!
//! ## Retry and escalation rules
//!
//! * **Structured output**: every structured agent goes through
//!   `vibe_agents::run_structured` (JSON extraction, repair call, one
//!   resume).
//! * **Plan**: an invalid plan (unknown or cyclic `depends_on`, duplicate
//!   titles, no subtask) is retried with the validation error as feedback,
//!   up to `max_phase_retries` times.
//! * **Build**: ready subtasks (explicit dependencies done, earlier plan
//!   phases finished) run up to `max_parallel_subtasks` at a time, each in a
//!   fresh coder session. A subtask is retried up to `max_subtask_attempts`
//!   times, the last attempt with the `coder_recovery` agent; it is then
//!   marked **failed** and its dependents **skipped**, and the build goes on.
//!   Each successful subtask is committed through the [`Committer`]. The
//!   build fails only when no subtask is done.
//! * **QA**: approved → merge. Changes requested → fix → QA again, at most
//!   `max_qa_rounds` reviews per invocation; then the run **pauses** with
//!   the task in **Review**. The same issue title in 3 consecutive rounds
//!   also pauses (escalation). An inconclusive review pauses too. Every
//!   pause publishes `Paused`.
//! * **Merge**: with `auto_merge`, the workspace is merged (**Done**);
//!   remaining conflicts leave the task **Ready** with the conflicting files
//!   in the progress notes. Without `auto_merge`, the task ends **Ready**.
//! * **Errors**: a phase error fails the run (**Failed**, `last_error` in
//!   `run.json`) and [`Pipeline::run`] still returns a [`RunReport`]; only
//!   infrastructure errors (store, workspace opening) are returned as `Err`.
//!
//! ## Resuming
//!
//! `run.json` records the phase being run, so [`Pipeline::resume`] continues
//! a crashed, cancelled, failed or paused run where it stopped. It reuses
//! the persisted spec and plan: subtasks already **done** are never redone
//! and interrupted ones are retried. A dry run ([`RunOptions::dry_run`]) or
//! [`RunOptions::until_phase`] leaves a paused run that `resume` continues.
//!
//! ## Persistence layout
//!
//! ```text
//! <project>/.vibe/tasks/
//! ├── index.json                  task id → { dir, number }
//! └── NNN-<slug>/
//!     ├── task.json
//!     ├── spec.json, spec.md
//!     ├── plan.json, plan.md
//!     ├── qa_report_<round>.json, qa_report_<round>.md
//!     ├── progress.md             append-only, timestamped notes
//!     ├── memory/gotchas.md, memory/patterns.md
//!     ├── events.jsonl            every event of every run
//!     └── run.json                state of the last run
//! ```

#![forbid(unsafe_code)]

pub mod complexity;
pub mod context;
pub mod kickoff;
pub mod phases;
pub mod pipeline;
pub mod state;
pub mod store;

pub use complexity::{
    AssessmentOutput, Profile, heuristic_complexity, parse_complexity, profile_for,
};
pub use context::{
    Committer, PhaseResult, ProviderResolver, RegistryResolver, RunContext, Transition,
};
pub use kickoff::{Kickoff, KickoffData, kickoff_for};
pub use pipeline::{Pipeline, PipelineDeps, RunOptions, RunReport, default_profile};
pub use state::{RunState, RunStatus};
pub use store::{
    FileEventSink, FileTaskStore, MemoryFile, PipelineStore, SharedPipelineStore, plan_to_markdown,
};
