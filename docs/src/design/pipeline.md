# The pipeline

`vibe-pipeline` turns a task into a sequence of agent sessions. It owns no provider, tool or
workspace: everything concrete is injected, so the same driver runs under the CLI, in tests
with a scripted provider, or inside another host. This page describes the driver as
implemented in `crates/vibe-pipeline/src/`; the on-disk artefacts it reads and writes are
described in [Persistence layout](persistence.md).

## Entry points

```rust
let pipeline = Pipeline::new(PipelineDeps { registry, providers, store, workspace,
                                            tools, events, config, project_root, committer });
let report: RunReport = pipeline.run(task_id, RunOptions::default()).await?;
let report = pipeline.resume(task_id).await?;            // continue the last run
let report = pipeline.resume_with(task_id, options).await?;
```

| `PipelineDeps` field | Role |
|----------------------|------|
| `registry: Arc<Registry>` | plugins, agent overrides, hooks, memory stores, plugin tools |
| `providers: Arc<dyn ProviderResolver>` | turns a `ModelRef` into a provider and a model id |
| `store: Arc<dyn PipelineStore>` | tasks, artefacts, run state, memory, event log |
| `workspace: Arc<dyn WorkspaceProvider>` | opens the task workspace, lists changes, merges |
| `tools: ToolRegistry` | built-in tools, merged with the registry's tools (the registry is cloned first, so a tool in `tools` wins on a name clash) |
| `events: EventBus` | where every event is published |
| `config: VibeConfig` | the parsed `.vibe/config.toml` |
| `project_root: PathBuf` | the project directory |
| `committer: Option<Committer>` | commits the workspace after subtasks and fixes |

| `RunOptions` field | Effect |
|--------------------|--------|
| `complexity_override` | forces a complexity; the heuristic and the assessor are skipped |
| `from_phase` | starts at this phase instead of `assess`, reusing persisted spec and plan |
| `until_phase` | pauses the run once this phase is done |
| `dry_run` | same as `until_phase = plan` (or an earlier `until_phase` if one is given) |
| `cancel` | a `watch::Receiver<bool>`; the run stops when it becomes `true` |

`RunReport` carries `run_id`, the saved `task`, `final_status` (a `TaskStatus`), every
`PhaseResult` in execution order (`phase`, `success`, `summary`, `usage`, `next`), the token
`usage` and wall-clock `duration` of this invocation, and the final `RunState`.
`is_success()` is true for `ready` and `done`.

A phase error does not make `run` return `Err`: the run ends as **failed**, `last_error` is
recorded in `run.json`, and a report is returned. Only infrastructure errors come back as
`Err`: the task cannot be loaded, the store cannot write, or the workspace cannot be opened
(in that last case `run_finished` is still published with `success: false`).

## Profiles

The complexity picks a `Profile` (`complexity.rs`): the phases run after `assess`, and which
spec steps run.

| Complexity | Phases after `assess` | Spec steps |
|------------|-----------------------|------------|
| `trivial` | plan, build, qa, merge | none: the planner works from the task description |
| `simple` | spec, plan, build, qa, fix, merge | gatherer only (light spec) |
| `standard` | spec, plan, build, qa, fix, merge | gatherer, writer |
| `complex` | spec, plan, build, qa, fix, merge | gatherer, researcher, writer, critic |

`fix` is never reached by walking the profile forward; only a QA verdict jumps to it.

The complexity is settled in this order:

1. `RunOptions::complexity_override` (`vibe run --complexity`);
2. the complexity already stored on the task (a task assessed once keeps its class);
3. `heuristic_complexity`: at most 30 words across title and description **and** a known
   small-change pattern. Typos, version or dependency bumps, colour, text, label, wording,
   title or copy changes and copyright-year updates are `trivial`; renames, removal of
   unused code, lint or compiler warning fixes and "add a comment" are `simple`;
4. the `complexity_assessor` agent. Aliases are accepted (`low`, `easy` → simple,
   `medium` → standard, `hard`, `high` → complex, …). Its `needs_research` and
   `needs_critique` flags switch on the researcher and critic, but only for profiles that
   write a full spec (standard and complex);
5. `standard`, when the assessor fails or answers an unknown class.

A run that starts after `assess` (`from_phase`) reuses the profile of the previous run when
there is one and no override is given; otherwise it applies steps 1–3 and 5 without calling
the assessor. `default_profile(task, override)` exposes the same computation for displays.

## Flow

```text
            ┌────────┐   ┌──────┐   ┌──────┐   ┌───────┐   ┌────┐  approved  ┌───────┐
 task ────▶ │ assess │──▶│ spec │──▶│ plan │──▶│ build │──▶│ qa │──────────▶│ merge │──▶ ready / done
            └────────┘   └──────┘   └──────┘   └───────┘   └────┘            └───────┘
                         (trivial                   │        │ ▲
                          skips it)                 │        │ │ changes requested
                                                    │        ▼ │
                                          no subtask done  ┌─────┐
                                                    │      │ fix │
                                                    ▼      └─────┘
                                                  failed      qa: round limit, inconclusive,
                                                              no fix phase; fix: same issue 3×
                                                              ──▶ paused, task in review
```

For every phase the driver:

1. checks the cancel token (cancelled → run **cancelled**);
2. sets the task status for the phase and saves `task.json` and `run.json`;
3. runs the `before_phase` hooks; an `Abort` ends the run as **cancelled** with the reason
   in `last_error` and in the progress notes;
4. publishes `phase_started`, runs the phase, publishes `phase_finished`;
5. runs the `after_phase` hooks and appends the phase to `completed_phases`;
6. follows the phase's `Transition`: `Continue` (next phase of the profile), `Goto` (QA ⇄
   fix) or `Stop { status, reason, pause }`;
7. applies `until_phase`, then saves `run.json` with the next phase as `current_phase`.

Each agent is resolved with `RunContext::agent_spec`: the registry's agent for the role
(plugins and `.vibe/agents/*.toml` overrides) or the built-in one. When
`[phases.<current phase>]` exists in the configuration, its `thinking` replaces the agent's
thinking level. The model is the agent's fixed model, else `config.model_for(phase)`. Tools
run with `ToolContext` rooted at the workspace and the permissions read, write and execute,
plus network when `security.allow_network` is set.

## Kickoff messages

The first user message of each session is built by `kickoff_for` from delimited sections.
When the agent's system prompt already consumes a section's placeholder (`{{spec}}`,
`{{plan}}`, …), the section is replaced by a one-line pointer, so the text is not sent twice;
custom prompts without placeholders still receive everything.

| Role | TASK | SUBTASK | SPEC | PLAN, PROGRESS NOTES | CHANGES | QA REPORT | MEMORY | PRIOR CONTEXT |
|------|:---:|:-------:|:---:|:--------------------:|:-------:|:---------:|:------:|:-------------:|
| `complexity_assessor` | yes | | | | | | | yes (empty) |
| `spec_gatherer`, `spec_researcher`, `spec_writer` | yes | | | | | | yes | yes |
| `spec_critic` | yes | | yes | | | | yes | yes |
| `planner` | yes | | yes | | | | yes | yes |
| `coder`, `coder_recovery` | yes | yes | yes | yes | | | yes | yes |
| `qa_reviewer` | yes | | yes | yes | yes | | yes | yes |
| `qa_fixer` | yes | | yes | | | yes | yes | yes |

A final `INSTRUCTIONS` section restates the JSON document the role must end with.
`PRIOR CONTEXT` carries: the gatherer JSON and research report for later spec steps; the
rejection reason for a planner retry; the subtask's failure history for a coder; a summary
of earlier rounds for QA and the fixer. Sizes are capped: prior context 12 000 characters,
progress notes 8 000 (most recent kept), memory 6 000, workspace changes 20 000 (beginning
and end kept), spec and plan 24 000. The memory section is the task's `memory/*.md` plus up
to 10 entries recalled from each registered `MemoryStore`.

## The phases

**assess** records the complexity on the task, the profile in `run.json`, and a progress
note naming the source (`override`, `stored on the task`, `heuristic`, `assessor …`,
`fallback …`).

**spec** runs the gatherer (mandatory: its failure fails the phase), then the researcher,
writer and critic as the profile says. Their failures are logged as warnings and the best
spec so far is kept. The writer also writes a markdown copy inside the workspace at
`.vibe/specs/<task short id>-spec.md`. A critic verdict `revised` with a non-empty spec
replaces the spec. An empty spec (no summary, no requirement) fails the phase. The spec's
`context.findings` are appended to `memory/patterns.md`.

**plan** runs the planner and converts its output with `plan_from_output`: subtask ids are
generated, `depends_on` entries are resolved by title (case-insensitive) or 1-based position,
and the plan is validated (no empty or duplicate title, no self or unknown dependency, no
cycle, at least one subtask). A flat `subtasks` list becomes one sequential phase called
`Implementation`. An invalid plan is retried with the error as feedback, up to
`max_phase_retries` more attempts, each announced by a `retrying` event.

**build** is the scheduler below. **qa** and **fix** are the loop below.

**merge** stops the run in all cases:

| Situation | Final task status |
|-----------|-------------------|
| `auto_merge = false` | `ready` |
| merged (fast-forward or merge commit), or nothing to merge | `done` |
| `MergeOutcome::NeedsHumanReview { files }` | `ready`, conflicting files in `progress.md` |
| merge error (for example a dirty project checkout) | `ready`, error in `progress.md` |

The conflict strategy is a property of the workspace provider the host builds; the pipeline
only records `pipeline.merge_strategy` in the progress notes.

## Build scheduler

A pending subtask is **ready** when every subtask in its `depends_on` is `done`, every
subtask of earlier plan phases is finished (`done`, `failed` or `skipped`), and, in a phase
whose `parallel` is false, the previous subtask of the same phase is finished. Up to
`max_parallel_subtasks` ready subtasks run at once, each in a fresh session spawned on its
own tokio task (sessions are aborted if the build ends early or its future is dropped).
Coder sessions use `run_with_continuation`, so a session that fills its context window is
summarised and continued.

A session succeeds when it ends with a JSON report whose `status` is `done` (or `success`,
`completed`, `ok`). Anything else is a failed attempt: the reason is appended to the
subtask's `notes`, which the next attempt receives as prior context. The attempt numbered
`max_subtask_attempts` uses `coder_recovery` instead of `coder` (when more than one attempt
is allowed). After the last attempt the subtask is **failed**, a line is added to
`memory/gotchas.md`, and every pending subtask depending on it through `depends_on`,
transitively, becomes **skipped**. Failures do not skip later phases by ordering alone.
Pending subtasks that can never become ready are skipped as "blocked by the plan ordering".

Commits are **deferred**: the committer stages the whole workspace, so completed subtasks
are committed only when no other session is running, possibly several in one commit
(`vibe: complete subtask 2 - Routes`, `vibe: complete subtasks 2, 3 - Routes; Login page`).
A failed commit is logged and noted, never fatal.

The build continues to QA when at least one subtask is done and none is pending; its
`success` flag is false if any subtask failed or was skipped. Otherwise the run fails with
`build failed: 0 done, 2 failed, 1 skipped`. Subtasks found `in_progress` when the build
starts (a crash) go back to pending.

## QA loop

Each QA invocation increments `qa_round` in `run.json`, gives the reviewer the workspace
change summary and the titles of earlier rounds, and saves `qa_report_<round>.json/.md`. A
reviewer that fails to produce valid JSON after repair and resume yields an `inconclusive`
report.

| Verdict | Next |
|---------|------|
| `approved` | merge |
| `changes_requested`, profile without `fix` (trivial) | pause, task `review` |
| `changes_requested`, `max_qa_rounds` reviews already done in this invocation | pause, task `review` |
| `changes_requested` otherwise | fix |
| `inconclusive` (including unknown verdict words) | pause, task `review` |

The fixer first checks for **escalation**: if one issue title (lowercased, whitespace
collapsed) appears in each of the last 3 reports, it records the issue in
`memory/gotchas.md` and pauses the run with the task in `review`. Otherwise the fixer runs,
the workspace is committed as `vibe: address QA round <n>`, and QA runs again. A pause
during `fix` leaves `current_phase = qa` so a resumed run reviews before fixing again. The
round limit counts reviews of the current invocation only, so each resume gets a fresh
budget, while `qa_round` keeps numbering reports across invocations.

## Statuses

| Phase running | `Task.status` |
|---------------|---------------|
| assess, spec, plan | `planning` |
| build | `building` |
| qa, fix, merge | `review` |

| End of run | `Task.status` | `RunStatus` |
|------------|---------------|-------------|
| merge, `auto_merge` off, conflicts or merge error | `ready` | `finished` |
| merge succeeded or nothing to merge | `done` | `finished` |
| QA pause or escalation | `review` | `paused` |
| `until_phase` or dry run reached | `backlog` | `paused` |
| phase error, build with nothing done | `failed` | `failed` |
| cancel token, hook abort | `cancelled` | `cancelled` |

A `RunStatus` of `running` in `run.json` after the process ended means it died mid-phase.

## Resume

`resume` loads `run.json` and refuses when there is none or its status is `finished`
(`start a new run`). `running`, `paused`, `failed` and `cancelled` are resumable. The run
keeps its `run_id` and profile, restarts at `current_phase` (or `from_phase` if given),
clears `last_error`, and reuses the persisted spec and plan. Subtasks already `done` are
never redone; interrupted ones are retried. When a **failed** run resumes at `build`, failed
and skipped subtasks go back to pending with their attempt counters reset, and a progress
note says how many.

`run` with `from_phase` is different: it starts a new run id at that phase, keeping only the
previous profile. It is how you re-run QA after fixing things by hand (`--from qa`).

## Events

The driver publishes `run_started`, `phase_started`, `phase_finished`, `subtask_updated`
(`in_progress`, `done`, `pending` after a failed attempt, `failed`, `skipped`), `retrying`
(plan retries with `delay_ms: 0`, subtask retries), `paused` and `run_finished`
(`success` true for `ready` and `done`); agent sessions add their own events (see
[Agent runtime](agent-runtime.md#events)). An internal router forwards each event carrying a
run id to that run's sink from the store, which appends it to `events.jsonl`.

## Seams

`ProviderResolver` maps a `ModelRef` to `(Arc<dyn ModelProvider>, model id)`. An empty
model (`ollama/`) means the provider's default. `RegistryResolver` looks providers up by
name in a `Registry`; the CLI adapts its provider registry, tests plug a scripted provider.

`Committer` is `Arc<dyn Fn(PathBuf, String) -> BoxFuture<'static, Result<Option<String>>>>`:
workspace root and message in, commit id (or `None` when there was nothing to commit) out.
Without a committer, nothing is committed during the run and the worktree provider commits
leftovers as `vibe: checkpoint` at merge time (see
[Workspaces and merging](../user/workspaces.md#merge-behaviour)).
