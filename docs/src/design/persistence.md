# Persistence layout

Vibe Factory keeps all of its state in plain files under `<project>/.vibe/`. There is no
database and no daemon: the artefacts on disk *are* the state machine. Each phase reads the
files of the previous ones and writes its own, which makes a run inspectable with `cat` and
`jq`, and resumable after a crash. This page lists every file, its shape, how it is written
and which ones you may edit.

## The `.vibe/` directory

```text
<project>/.vibe/
├── config.toml                     project configuration
├── agents/*.toml                   agent overrides and custom agents (+ prompt files)
├── plugins/<name>/vibe-plugin.toml plugin manifests
├── memory.jsonl                    project memory: one lesson per line, shared by tasks
├── server.token                    token of a running `vibe serve` (owner-only)
├── tool-output/<task dir>/<run>/<call>.txt   complete output of every tool call (trace store)
├── worktrees/                      one git worktree per task (git_worktree workspace)
│   ├── .gitignore                  "*"
│   └── <slug>-<id8>/               the task's workspace
│       └── .vibe/
│           ├── specs/<id8>-spec.md markdown copy written by the spec writer
│           └── tool-output/<uuid>.txt   full text of truncated tool outputs
└── tasks/
    ├── index.json                  task id → directory and number
    ├── .index.lock                 OS lock serialising index updates across processes
    └── NNN-<slug>/                 one directory per task
        ├── task.json
        ├── spec.json   spec.md
        ├── plan.json   plan.md
        ├── qa_report_1.json   qa_report_1.md      one pair per QA round
        ├── progress.md                           append-only, timestamped
        ├── memory/gotchas.md   memory/patterns.md
        ├── events.jsonl                          every event of every run
        ├── run.json                              state of the last run
        ├── run.lock   run.owner                  OS lock and pid of the process running it
        └── cancel.request                        present while a `vibe cancel` is pending
```

| Path | Written by | Read by |
|------|------------|---------|
| `config.toml` | `vibe init`, `vibe config set`, you | every command ([Configuration](../user/configuration.md)) |
| `agents/` | you, `vibe agents export` | agent loading ([Customising agents](../user/agents.md)) |
| `plugins/` | you, plugin installers | plugin discovery ([Plugins](../user/plugins.md)) |
| `worktrees/` | the `git_worktree` workspace provider | agents, you ([Workspaces](../user/workspaces.md)) |
| `tasks/` | `FileTaskStore` (`vibe-pipeline`) | the pipeline, `vibe task`, `vibe status` |

The `tool-output/<uuid>.txt` files and the `specs/` directory live in the *workspace*, so with
the `in_place` workspace they appear directly under `<project>/.vibe/`. The trace store
`<project>/.vibe/tool-output/<task dir>/<run>/` is always at the project root: one file per
tool call, named by the call id of its `tool_called`/`tool_returned` events and capped at
`pipeline.trace_max_chars` characters (see [Events](../reference/events.md#trace-store)).
`pipeline.trace_outputs = false` turns it off; `vibe task discard` removes the task's
directory. Workspace change summaries and the
merge checkpoint commit both exclude `.vibe/`.

## The task store

`FileTaskStore::open(project_root)` creates `.vibe/tasks/` if needed. A task gets its
directory the first time it is saved: the next sequence number, zero-padded to three digits,
and the slug of its title (lowercase ASCII alphanumerics, other runs replaced by `-`, at most
48 characters, `task` if empty). The number and the directory never change afterwards, even
if the title does.

### `index.json`

```json
{
  "next_number": 2,
  "tasks": {
    "6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19": { "dir": "001-add-oauth-login-github", "number": 1 },
    "a4b21f77-0c9e-4d3b-b8f1-2e6a9c0d4e58": { "dir": "002-fix-typo-in-readme", "number": 2 }
  }
}
```

A new number is `max(next_number, highest number in use) + 1`, so deleting the last task
does not reuse its number. User references are resolved by `find_by_prefix`: a number of up
to six digits (`3`, `003`), an exact directory name (`003-add-login`), or a prefix of the
task id; an ambiguous prefix is an error. A corrupt index is reported as
`corrupt task index <path>: …` rather than silently rebuilt.

### Files of a task

| File | Content | Written |
|------|---------|---------|
| `task.json` | the `Task` | on creation and at every status change |
| `spec.json`, `spec.md` | the `Spec`, and its rendering | end of the spec phase |
| `plan.json`, `plan.md` | the `Plan` with subtask statuses, attempts and notes | end of plan, then after every subtask state change |
| `qa_report_<n>.json`, `.md` | the `QaReport` of round `n` | after each QA round |
| `progress.md` | timestamped notes from every phase | appended throughout |
| `memory/patterns.md` | spec findings | appended at the end of spec |
| `memory/gotchas.md` | repeatedly failing subtasks, escalated QA issues | appended when they happen |
| `events.jsonl` | one `Envelope` per line | appended by the run's event sink |
| `run.json` | the `RunState` of the last run | before and after every phase, and at the end |
| `run.lock`, `run.owner` | empty lock file, pid | taken by the process running the task, released when it ends or dies |
| `cancel.request` | marker | written by `vibe cancel`, consumed by the running process |

The shapes of `Task`, `Spec`, `Plan` and `QaReport` are documented in
[Domain model](domain-model.md). A `task.json`:

```json
{
  "id": "6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19",
  "title": "Add OAuth login (GitHub)",
  "description": "Users should be able to sign in with their GitHub account.",
  "status": "ready",
  "complexity": "standard",
  "labels": ["auth"],
  "source": { "kind": "manual" },
  "created_at": "2026-09-27T08:12:03.114Z",
  "updated_at": "2026-09-27T08:41:57.902Z",
  "branch": "vibe/add-oauth-login-github-6f1c3e0a"
}
```

`branch` is recorded when a run opens a workspace that has one (absent with `in_place`);
`vibe pr`, `vibe task show` and `vibe task discard` use it instead of recomputing the branch
from the title.

`plan.md` marks subtasks with `[ ]` pending, `[~]` in progress, `[x]` done, `[!]` failed and
`[-]` skipped, and shows dependencies by title, the attempt count, verification steps and
notes. Markdown files are always rendered from the JSON and never parsed back.

### `run.json`

```json
{
  "run_id": "a3f09c2e-5d71-4b8e-9f06-1c2d3e4f5a6b",
  "task_id": "6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19",
  "current_phase": "qa",
  "completed_phases": ["assess", "spec", "plan", "build", "qa", "fix", "qa", "fix"],
  "qa_round": 3,
  "profile": {
    "phases": ["spec", "plan", "build", "qa", "fix", "merge"],
    "research": false,
    "critique": false,
    "full_spec": true
  },
  "started_at": "2026-09-27T08:12:10.520Z",
  "updated_at": "2026-09-27T08:39:44.061Z",
  "status": "paused",
  "last_error": "issue `login button has no accessible label` was reported in 3 consecutive QA rounds; escalating to a human"
}
```

| Field | Meaning |
|-------|---------|
| `run_id` | identifier of the run, kept across resumes |
| `current_phase` | the phase executing, or the next one to execute when the run is not `running` |
| `completed_phases` | phases that completed, in order; `qa` and `fix` may repeat |
| `qa_round` | QA reviews performed so far; numbers the report files |
| `profile` | the profile chosen by the assessment (absent until known) |
| `status` | `running`, `paused`, `finished`, `failed` or `cancelled` |
| `last_error` | why the run stopped, for any status other than `finished` |

Only the **last** run of a task is kept: a new `vibe run` overwrites `run.json`. Its events
remain in `events.jsonl`, which is never truncated and can be split by `event.run`:

```sh
jq -c 'select(.event.type == "phase_finished") | .event' .vibe/tasks/001-*/events.jsonl
```

Only events carrying the run's id are written; `log` events without a run id are not.

## Writing strategy

Every JSON and markdown artefact is written **atomically**: the content goes to a uniquely
named temporary file in the same directory (`.<name>.<uuid>.tmp`), which is then renamed over
the target. A crash therefore leaves either the old or the new file, never a truncated one;
at worst a stray `.tmp` file, safe to delete. JSON is pretty-printed with a trailing newline.

Appends (`progress.md`, memory files) and index updates are serialised by locks inside one
process; `events.jsonl` has its own lock per sink. There is **no cross-process locking**: do
not run two `vibe` processes on the same task at once. Different tasks are independent,
except for `index.json`, so avoid creating tasks from two processes at the same moment.

Missing files are normal: a task without `spec.json` simply has no spec yet, and loading it
returns `None`. A file that exists but does not parse is an error naming the path, so a bad
hand edit is reported instead of ignored.

## What you may edit

| File | Safe to edit? | Notes |
|------|---------------|-------|
| `config.toml`, `agents/*`, `plugins/*` | yes | read at the start of each command |
| `progress.md` | yes | agents read its last 8 000 characters; add guidance for the next run |
| `memory/gotchas.md`, `memory/patterns.md` | yes | fed to every agent except the assessor (6 000 characters) |
| `task.json` | with care | editing `description` changes what the next run sees; set `complexity` to `null` to have it re-assessed |
| `spec.json`, `plan.json` | with care | must stay valid against the types; edit the JSON, not the `.md`, which is regenerated; a plan must still pass validation |
| `qa_report_<n>.json` | no | the escalation check reads the titles of the last three rounds |
| `run.json` | no | delete it to forget the last run instead |
| `index.json`, `events.jsonl` | no | use `vibe task discard` to remove a task |

Stop any run of the task before editing its files.

## What to commit

In a git repository, `vibe init` creates (or completes, without duplicating lines)
`.vibe/.gitignore`:

```gitignore
# tasks/ is committed on purpose: specs, plans and QA reports are project history.
worktrees/
tool-output/
```

Task directories are therefore committed by default: reviewers see the spec, plan and QA
reports next to the change they produced.

| Commit | Ignore |
|--------|--------|
| `config.toml` (without inline `api_key` values) | `worktrees/` (ignored by `vibe init`) |
| `agents/` | `tool-output/` (ignored by `vibe init`: the trace store, plus truncated outputs with `in_place`) |
| `plugins/` manifests you want everyone to load | `specs/` (only appears with `in_place`) |
| `tasks/` (the default) | optionally `tasks/*/events.jsonl` and `tasks/*/run.json` |

The event log is large and machine-oriented, and `run.json` changes on every run; add them
to `.vibe/.gitignore` if the noise bothers you. To keep all task data local, add `tasks/`.

`.vibe/worktrees/` also contains its own `.gitignore` with `*`, written when the first
worktree is created, so worktrees never show up in your `git status` even without these
entries.

## How a resume reads the state

`Pipeline::resume` needs only the task directory:

1. `task.json` gives the task and its stored complexity;
2. `run.json` gives the run id, the profile, the phase to restart at, and whether the run is
   resumable (anything but `finished`);
3. `spec.json` and `plan.json` are loaded if present, so spec and plan are not redone;
4. in `plan.json`, `done` subtasks are kept, `in_progress` ones go back to pending, and
   after a failed build `failed` and `skipped` ones get a fresh attempt budget;
5. `qa_report_*.json` provide the earlier rounds to QA and to the escalation check;
6. `progress.md` and `memory/` feed the agents' context as in any run.

The workspace is reopened by the workspace provider (the same worktree and branch), so the
commits made by earlier attempts are still there. See [The pipeline](pipeline.md#resume) for
the exact rules.
