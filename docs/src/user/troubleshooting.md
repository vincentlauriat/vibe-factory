# Troubleshooting

Each section starts from what you see, explains the cause, and gives the fix. Two commands
answer most questions:

```sh
vibe task show <ref>     # status, last run, QA verdict, workspace
vibe doctor              # environment, configuration, keys, plugins
```

## Where to look

| Source | What it tells you |
|--------|-------------------|
| `.vibe/tasks/NNN-slug/progress.md` | a timestamped note for every step: assessment, spec, plan, each subtask attempt with its failure reason, each QA round, merge result, pause reasons |
| `.vibe/tasks/NNN-slug/run.json` | the last run: `status`, `current_phase`, `completed_phases`, `qa_round`, `last_error` |
| `.vibe/tasks/NNN-slug/events.jsonl` | every event of every run: tool calls with their input, tool results, retries, agent stops |
| `.vibe/tasks/NNN-slug/plan.md` | each subtask's status, attempts and notes |
| `.vibe/tasks/NNN-slug/qa_report_<n>.md` | the issues of each QA round |
| `vibe history <ref>` | what each run did: phases, commits, changed files, validations, QA verdict, errors ([History](history.md)) |
| `vibe trace <ref> [--full]` | every tool call of a run with its arguments, exit code and complete output ([Tool call trace](trace.md)) |
| `vibe events --since 1h` | the recent activity of every task, in time order |
| standard error with `-v`, `-vv`, `-vvv` | logs at info, debug, trace level; `RUST_LOG` overrides |

```sh
vibe trace 3 --tool bash --full      # every shell command of the last run, with its output
jq -r 'select(.event.type == "tool_returned" and .event.is_error) | "\(.event.tool): \(.event.preview)"' \
  .vibe/tasks/003-*/events.jsonl
RUST_LOG=vibe_agents=debug,vibe_plugins=debug vibe run 3 --resume 2> run.log
```

The file layout is described in [Persistence layout](../design/persistence.md).

## The first model call fails with `AuthFailed`

```text
AuthFailed: no API key configured for provider `anthropic` (set `api_key` or `api_key_env`)
AuthFailed: HTTP 401: invalid x-api-key sk-***
```

**Cause.** The provider's key is missing, empty or wrong. Providers load without a key and
only fail on their first call, so this appears at the start of `assess` or `spec`.

**Fix.** Export the variable named by `api_key_env` in the shell that runs `vibe`
(`ANTHROPIC_API_KEY` or `OPENAI_API_KEY` for the built-ins), or switch to a provider you have
(`vibe run 3 --provider ollama`). `vibe doctor` lists every declared provider, whether the configuration uses it, and
whether their keys are set. Then resume: `vibe run 3 --resume`.

`InvalidRequest: HTTP 429: You exceeded your current quota` is a billing problem, not a rate
limit: it is not retried. Check your account.

## `Retrying` lines and rate limits

```text
  ↻ retrying model call (claude-sonnet-5): HTTP 429: rate limit reached for requests (attempt 2, in 30.0 s)
```

**Cause.** A transient failure: rate limit (429), overloaded or server error (5xx), network
error or timeout. Providers first retry silently, four attempts with exponential backoff
(1 s doubling up to 60 s, honouring `Retry-After`); if the call still fails, the agent
runtime retries it twice more, and those retries are the lines you see. Nothing to do unless
it keeps happening.

**Fix, if it persists.** Lower `pipeline.max_parallel_subtasks` (each parallel subtask is a
concurrent session), use a smaller model for high-volume phases (`[phases.build]`), or run
fewer tasks at once. If all retries fail, the phase fails; resume the run later.

`Retrying` also appears for pipeline-level retries: `plan: <validation error>` when the
planner produced an invalid plan, and ``subtask `…` `` before another attempt of a subtask.

## `ContextTooLong` and continuation

**Cause.** A session grew past the model's context window (many large files read, long
command outputs). The provider reports it as `ContextTooLong` and the session stops with
`context_window`.

**What happens.** Coder sessions (build) continue automatically: the transcript is summarised
into handover notes and a fresh session resumes from them, up to five times. Other agents do
not continue; their session ends with whatever they produced, which usually leads to the JSON
problem below.

**Fix.** Use a model with a larger context for the phase, split the task, or make subtasks
smaller by asking for it in the description. Large tool outputs are already truncated and
saved under `.vibe/tool-output/`.

## A model returns invalid JSON

```text
agent `planner` did not produce valid structured output after a retry: no JSON object or array found in the output
agent `qa_reviewer` did not produce valid structured output after a retry: JSON does not match the expected shape: invalid type: string "none", expected a sequence
```

**Cause.** Structured agents must end with a JSON document. When extraction fails, the
runtime makes one repair call, then resumes the agent once asking for valid JSON. A second
failure is an error. What follows depends on the role:

| Role | Effect of the error |
|------|---------------------|
| `complexity_assessor` | the task falls back to `standard` |
| `spec_gatherer` | the spec phase fails; the run is `failed` |
| `spec_writer`, `spec_critic` | ignored; the spec keeps the gatherer's requirements (or the writer's, for the critic) |
| `planner` | counts as a failed attempt; retried with the error as feedback, up to `max_phase_retries` more times |
| `coder`, `coder_recovery` | a failed attempt of the subtask |
| `qa_reviewer` | the round is `inconclusive`: the run pauses with the task in `review` |

**Fix.** Small local models are the usual cause: use a stronger model for the phase
(`[phases.plan]`, `[phases.qa]`), raise `max_tokens` of the agent if answers are cut, and see
[Writing prompts that return valid JSON](agents.md#writing-prompts-that-return-valid-json)
if you replaced a prompt. Then `vibe run <ref> --resume`.

## A subtask fails three times

```text
  [2/4] Wire the export route                              failed
✓ build  3 done, 1 failed, 0 skipped
```

**Cause.** Each attempt ended without a coder report saying `done`. The third attempt
(`max_subtask_attempts`) used the recovery agent. The reasons are in `plan.md` (the
subtask's notes) and `progress.md`; `memory/gotchas.md` records the failure for later runs.
Subtasks that depend on it are `skipped`.

**What happens next.** If at least one subtask is done and none is pending, the run goes on
to QA, which will likely request changes for the missing part. If nothing is done, the run
fails with `build failed: …`.

**Fix.** Read why it failed. Often a verification command cannot pass in the worktree
(missing service, missing tool, command denied). Fix the cause, then:

```sh
vibe run 3 --resume     # a failed build retries failed and skipped subtasks with a fresh budget
```

To finish the subtask yourself, do it in the worktree, commit, set its `status` to `"done"`
in `plan.json`, and run `vibe run 3 --from qa`.

## QA never approves

```text
⏸ paused: QA did not approve after 3 round(s); human review needed
⏸ paused: issue `export ignores --limit with --json` was reported in 3 consecutive QA rounds; escalating to a human
⏸ paused: QA round 2 was inconclusive; human review needed
```

**Cause.** The run pauses with the task in `review` (exit code 2) when QA still requests
changes after `max_qa_rounds` reviews in this invocation, when the same issue title appears
in three consecutive reports, when a review is inconclusive, or when a `trivial` task, which
has no fix phase, gets changes requested.

**Fix.** Read the latest `qa_report_<n>.md`. Then either:

- **let the agents try again**: `vibe run 3 --resume` resumes at QA with a fresh round budget.
  An escalated issue that is still reported three rounds in a row pauses again, so change
  something first (a hint in `progress.md`, a stronger model for `fix`);
- **fix it by hand**: edit and commit in the worktree
  (`.vibe/worktrees/<slug>-<id>/`), then `vibe run 3 --from qa` for a new review;
- **accept it as is**: merge the branch yourself (see [Workspaces](workspaces.md)).

If QA is wrong about a requirement, correct the requirement in `spec.json` before re-running
QA; the reviewer checks acceptance criteria literally.

## Merge conflicts

```text
✓ merge  merge conflicts in 2 file(s)
Task 3 is ready · branch vibe/add-oauth-login-github-6f1c3e0a
```

**Cause.** With auto-merge, the base branch moved and the task branch conflicts with it. The
workspace provider returned `NeedsHumanReview`: the merge was aborted, your checkout was put
back on its branch and left clean, and the task is `ready` with the conflicting files listed
in `progress.md`. The worktree and branch are left in place.

**Fix.** Merge by hand and resolve as usual:

```sh
git switch main
git merge vibe/add-oauth-login-github-6f1c3e0a
```

## The merge is refused because the project is dirty

```text
Automatic merge failed: cannot merge vibe/… : the project at /repo has uncommitted changes; commit or stash them first
```

**Cause.** Auto-merge works in your project checkout, and tracked files there have
uncommitted modifications. Untracked files do not block it. The task ends `ready`.

**Fix.** Commit or stash your changes, then merge the branch by hand, or run
`vibe run 3 --from merge --auto-merge`.

## The worktree already exists

**Cause.** A task keeps its worktree between runs; running it again reuses it. A worktree
directory that git does not know about (for example restored from a backup) is removed and
recreated; a branch whose worktree was deleted gets a new worktree with its commits.

**Fix.** Usually nothing. If git itself complains that the branch is checked out elsewhere
or the worktree is locked, clean up and retry:

```sh
git worktree prune
git worktree list
git worktree remove --force .vibe/worktrees/<slug>-<id>   # only if you want to start over
```

A project that is not a git repository fails before the run starts with
``/path is not a git repository: run `git init`, or use `--workspace in_place` ``.

## A plugin fails to start

```text
skipping plugin `jira`: Plugin: cannot start plugin `jira` (`jira-vibe-plugin` in `/repo`): No such file or directory
required plugin `policy` failed to start: …
```

**Cause.** The program is not on `PATH`, not executable, or crashes during the handshake. An
optional plugin is skipped with a warning and the run continues without its tools; a plugin
with `required = true` stops the run. The handshake times out after 60 seconds, a tool call
after 600.

**Fix.** Run `vibe plugins check`: it starts each plugin, prints what it offers or the
error, and stops it. Plugin standard error is logged at debug level, so add `-vv`. Details in
[Plugins](plugins.md#troubleshooting).

## A command is denied by the security policy

```text
bash ⟵ error: Command denied by the security policy: `rm` may not target `/`
```

**Cause.** The shell policy blocked a segment of the command. The agent receives the reason
and usually finds another way, so a single denial is not a problem. Repeated denials of a
command the task really needs (a network client, `docker`, a database client) will make
subtasks or QA fail.

**Fix.** Allow what is needed in `[security]`: `allow_network = true` for `curl` and friends,
remove the program from `blocked_commands`, or add it to `allowed_commands` if you use an
allowlist. System administration programs (`sudo`, `mount`, …) are always blocked. The rules
are listed in [Tools and security](security.md).

## `vibe` says your home directory is not a git repository

```text
error: /Users/me is not a git repository: run `git init`, or use `--workspace in_place`
```

**Cause.** Before 0.4.0, `vibe` looked for the nearest parent holding a `.vibe` directory
without limit, and a `~/.vibe` left by another tool made it take your home directory for the
project. Since 0.4.0 the search stops at the git repository and never climbs up to the home
directory; running `vibe` from the home directory itself still uses it, like any other
directory.

**Fix.** Check which binary runs (`vibe --version`, `which -a vibe`) and reinstall if it is
older than 0.4.0. With a current version, the project is the directory you are in (or the
nearest parent with `.vibe` inside the same repository): run `vibe` from inside the
repository, or name it with `-C <dir>`. The other tool's `~/.vibe` does not need to be
removed.

## `.vibe/tool-output/` keeps growing

**Cause.** The trace store keeps the complete output of every tool call of every run, up to
`pipeline.trace_max_chars` characters per call (100 000 by default), and nothing removes it
automatically. Long test suites and verbose builds fill it fastest.

**Fix.**

```sh
du -sh .vibe/tool-output/*/                  # which tasks use the space
vibe task discard 3 --yes                   # a task you no longer need, with its outputs
rm -rf .vibe/tool-output/003-*/<run>        # the outputs of one old run only
```

To keep less, lower `pipeline.trace_max_chars`; to keep nothing but the previews in the log,
set `pipeline.trace_outputs = false`. Removed outputs only make `vibe trace --full` fall back
to the preview. The directory is ignored by git. Details in
[Tool call trace](trace.md#disk-usage).

## A `vibe serve` started by an application is still running

```text
error: cannot listen on 127.0.0.1:7777
  caused by: Address already in use (os error 48)
```

**Cause.** An application (an editor, the macOS app, a script) started `vibe serve` as a
child process and died without stopping it, for example after a crash or a `kill -9`. The
orphan server keeps its port, its token file `.vibe/server.token`, and possibly runs.

**Fix.** Find it and stop it with `SIGINT`, which cancels its runs cleanly (they stay
resumable) and removes the token file:

```sh
pgrep -fl "vibe.*serve"
kill -INT <pid>
```

An application that starts `vibe serve` should pass `--exit-on-stdin-eof` and keep a pipe
on the child's standard input: when the application ends, however it ends, the pipe closes
and the server stops as on `Ctrl-C`. The macOS app does this. For a server you start by
hand, `--port 0` picks a free port.

## `vibe history` shows `~`, `+` or `-`

```text
#  title          status  runs  commits  files  tokens  active      cost  finished
4  Add a cache    done    2     3        5~     84.1k+  3 min 10 s+ -     2 d ago
```

These marks are not errors:

- **`~` on files**: the list of changed files is approximate. The task branch is gone and
  no merge was recorded, so the list comes from the commit events or from the files the
  agents wrote. `vibe history <ref>` names the source.
- **`+` on tokens or active time**: a lower bound. A run was logged before 0.5 (its totals
  are unknown) or has not finished; `vibe history <ref>` shows which run.
- **`+` on the cost**: some tokens were used outside a finished agent session (a session cut
  by a crash, or the repair of an invalid structured answer) and are not priced.
- **`-` as the cost**: there is no [`[pricing]`](configuration.md#pricing) table, or a model
  the task used has no price in it (a local model needs a price of zero), or the task ran
  before 0.5 and its models are not recorded.

The detail also lists **problems** met while reading, such as a merge commit that git no
longer has (after a rebase or a garbage collection): the history then falls back to the next
source. See [History](history.md#changed-files-and-the-approximate-marker).

## Windows specifics

- Shell commands run through `cmd /C` but are validated with POSIX shell rules, so `cmd`
  syntax such as `%VAR%` or `^` escapes may be denied as unparseable. Ask for plain commands
  in the task description, or pin the project's commands in `.vibe/agents/coder.toml`.
- Deep worktree paths can exceed 260 characters: `git config --global core.longpaths true`
  and enable long paths in Windows.
- Antivirus scanners can lock files in `target/` and make renames fail; exclude the project's
  `.vibe/worktrees/` directory.

## The run was interrupted

A crash or a killed terminal leaves `run.json` with `"status": "running"`. `Ctrl-C` leaves it
`cancelled`. Both are resumable:

```sh
vibe run 3 --resume
```

Subtasks that were in progress go back to pending; completed ones are kept.

## Resetting a task

| Goal | Do |
|------|----|
| re-run QA after manual changes | `vibe run 3 --from qa` |
| re-plan from the same spec | `vibe run 3 --from plan` |
| redo the spec after editing the description | edit `description` in `task.json`, then `vibe run 3 --from spec` |
| re-assess the complexity | set `"complexity": null` in `task.json`, or pass `--complexity`, then `vibe run 3` |
| forget the last run | delete `run.json`; artefacts stay |
| start completely over | `vibe task discard 3 --yes`, then `vibe task add` again |

A task keeps its complexity once assessed, so a plain `vibe run 3` never calls the assessor
again. `--from` starts a new run that reuses the files already on disk; delete `spec.json` or
`plan.json` if you want them regenerated rather than reused by later phases.
