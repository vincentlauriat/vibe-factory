# History of finished work

`vibe history` answers "what did the agents deliver, and what did it cost?" for every task,
long after the run that did the work. Nothing is asked of a model and nothing extra is
stored: the history is rebuilt each time from what the runs already left in `.vibe/` and in
git ([ADR-008](../design/adr/008-trace-store-and-read-layer.md)).

## The table

```sh
vibe history            # ready and done tasks, most recent first
vibe history --all      # failed and cancelled tasks too
```

```text
#  title                      status  runs  commits  files  tokens  active  cost  finished
2  Write the greeting module  done    1     1        1      6.7k    251 ms  -     just now
1  Fix typo in README         done    1     0        0      2.9k    187 ms  -     just now
```

| Column | Meaning |
|--------|---------|
| `runs` | runs of the task; a run and its resumes count once |
| `commits` | commits the pipeline made for the task (checkpoints, subtasks, fixes, integrations) |
| `files` | files the task changed, `~` when the list is approximate (see below) |
| `tokens` | input plus output tokens of every run, resumes included |
| `active` | time the runs were working, pauses excluded |
| `cost` | money spent, only with a [`[pricing]`](configuration.md#pricing) table; `-` otherwise |
| `finished` | end of the last run, else the last activity |

A `+` after `tokens`, `active` or `cost` marks a lower bound: a run was logged before 0.5,
whose totals are unknown, or has not finished, or some tokens were used outside a finished
agent session and are not priced. A legend under the table repeats this when it applies.

## One task in detail

```sh
vibe history 2
```

```text
#2 Write the greeting module
  status    done
  branch    vibe/write-the-greeting-module-6fed323c
  tokens    6.3k in / 365 out
  active    251 ms
  activity  just now

Runs (1)
  4ef9f554 finished → done, started just now
    251 ms active, 6.3k in / 365 out tokens
    phases  assess ✓  spec ✓  plan ✓  build ✓  qa ✓  merge ✓
    merged  vibe/write-the-greeting-module-6fed323c into main (62a82acb)

Commits (1)
  62a82acb  vibe: complete subtask 1 - Implement the task (1 file(s)) · subtask 1/1 Implement the task

Files (1) from merge 62a82acb (fast-forward)
  A  hello.txt

QA round 1: approved, 0 issue(s)
  Approved by the mock provider.
```

Any task can be shown, finished or not. Each run shows its state: `finished`, `running`
when a process holds the task's run lock, or `interrupted` when the process died before the
run ended (`vibe run <REF> --resume` continues it). The detail ends with the validation
commands of the last run that ran them, the last QA verdict, and the problems met while
reading (a merge commit git no longer has, a file that does not parse), if any.

## Where the data comes from

| Part | Source |
|------|--------|
| runs, phases, approvals, errors | the `run_started` … `run_finished` events of `events.jsonl`; `run.json` completes the last run |
| tokens, active time | the totals of each run's last `run_finished` (resumes included); `run.json` for the last run when its log predates 0.5 or it has not finished |
| commits | the `committed` events (and `subtask_integrated` for older logs) |
| merge | the `merged` event |
| validations | the `validation_finished` events |
| QA verdict | the last `qa_report_<n>.json` |
| changed files | git, else the events (below) |
| cost | the `agent_started` model and `agent_finished` usage of each session, and `[pricing]` |

### Changed files and the `approximate` marker

The list of changed files comes from the most exact source available, and the detail says
which one it used (`from …`):

1. **the recorded merge** (`merge <sha>`): the files the merge brought into the base
   branch. A fast-forward is compared from where the task's first recorded commit started;
2. **the task branch** (`diff of <branch> against <base>`), while it exists and differs from
   its base;
3. **the commit events** (`commit events`): every file of every `committed` event, statuses
   unknown;
4. **the files written by the agents** (`files written by the agents`): paths passed to
   `write_file` and `edit_file` calls that succeeded;
5. `nothing recorded`.

Sources 3 and 4 are **approximate**: they can list a file a later commit reverted, and the
fourth misses files that shell commands wrote. The first is approximate in one case: a
fast-forward that contains none of the task's recorded commits. Approximate lists are
marked `~` in the table and `approximate` in the detail. Statuses are `A` added, `M`
modified, `D` deleted, `R` renamed, `C` copied, `?` unknown. The rules are detailed in
[Persistence layout](../design/persistence.md#reading-the-store-the-read-layer).

A project outside git, or a task run with the `in_place` workspace, has no branch to diff:
its files come from the events.

## Tokens, time and cost

Tokens and active time are always shown. A money cost is shown only when you give prices:

```toml
[pricing."anthropic/claude-sonnet-5"]
input = 3.0
output = 15.0
cache_read = 0.3
cache_write = 3.75
```

The cost of a task is computed only when every agent session it ran used a model with a
price that covers its tokens; otherwise it is `-`, never an estimate. Matching rules and the
cases that make a cost a lower bound are in [`[pricing]`](configuration.md#pricing).

## JSON and other interfaces

`vibe --json history` prints an array of `TaskHistory` documents, `vibe --json history <REF>`
one of them; the fields are listed in the
[command line reference](cli.md#vibe-history). The same documents are served by
`GET /api/history?all=bool` and `GET /api/history/{ref}` ([`vibe serve`](cli.md#vibe-serve)),
shown by the **History** view of the web UI (a table whose rows expand to the detail) and by
the **History** screen of `vibe tui` (`H`). Clients of `vibe serve`, such as the macOS app
in [`apps/macos/VibeFactory`](https://github.com/vincentlauriat/vibe-factory/tree/main/apps/macos/VibeFactory),
read the same routes.

To see *how* a run got there, call by call, use [`vibe trace`](trace.md).
