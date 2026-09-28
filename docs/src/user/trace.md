# Tool call trace

`vibe trace` shows how a run did its work: every tool call an agent made, with its complete
arguments, how long it took, how it ended, and its output. It is the answer to "which
command failed, and what did it print?" once the live output of `vibe run` is gone.

## Reading a trace

```sh
vibe trace 2                     # the last run of task 2
vibe trace 2 --full              # with complete arguments and outputs
vibe trace 2 --tool bash         # only the shell commands
vibe trace 2 --all               # every run of the task
vibe trace 2 --run 4ef9          # one run, by id or id prefix
vibe trace 2 --subtask 7c1e      # the calls made for one subtask
```

```text
── run 4ef9f554 ──────────

#1 coder · subtask 1/1 Implement the task · write_file  1 ms  ok  [call 2e5c7f564a52]
    {
      "content": "Hello, world!\n",
      "path": "hello.txt"
    }
  ⟵ output
    Created `hello.txt` (14 bytes).

#2 coder · subtask 1/1 Implement the task · bash  7 ms  error, exit 1  [call 4b5fd969429a]
    {
      "command": "cat hello.txt; ls nope"
    }
  ⟵ output
    Hello, world!
    ls: nope: No such file or directory

    Exit code: 1

2 call(s), 1 error(s)
files written: hello.txt
```

Each block starts with the call's position in the run, the agent role, the subtask (its
position and title in the plan), the tool, the duration and the result: `ok`, `exit N` for a
command, `error`, `timed out`, or `no result` when the run was interrupted before the tool
returned. The call id at the end pairs the call with its result. The footer counts the calls
and errors and lists the files written by `write_file` and `edit_file`. The options are
described in the [command line reference](cli.md#vibe-trace).

Without `--full`, the output is the 200-character preview logged with the event, and
strings longer than 2 000 characters in the arguments are cut. `--full` reads the complete
output from the trace store and prints the arguments whole.

## The trace store

While a run works, the complete output of every tool call is written to

```text
.vibe/tool-output/<task dir>/<run>/<call>.txt
```

at the project root, for example
`.vibe/tool-output/002-write-the-greeting-module/4ef9f554-…/4b5fd969429a.txt`. The
`tool_returned` event records that path (`output_file`) and the length of the complete
output (`output_chars`), so the log stays small and the output stays one read away
([Events](../reference/events.md#trace-store)).

Two settings control it ([`[pipeline]`](configuration.md#pipeline)):

| Key | Default | Effect |
|-----|---------|--------|
| `trace_outputs` | `true` | write the store; `false` keeps only the previews in the log |
| `trace_max_chars` | `100000` | characters kept per call; a longer output is cut and ends with `[vibe: output cut to the first N of M characters]` |

The store is independent of what the model sees. An output longer than the runner's limit
is still truncated for the model, and its complete text is also written in the workspace
(`<workspace>/.vibe/tool-output/<uuid>.txt`) where the agent can read it, so such outputs
exist twice.

### Disk usage

The store grows with what the tools print: at most `trace_max_chars` characters per call,
usually far less (a `write_file` result is one line; a test suite's output can reach the
cap). Nothing is removed automatically. To see and reclaim the space:

```sh
du -sh .vibe/tool-output/*/                      # per task
rm -rf .vibe/tool-output/002-write-the-greeting-module/<run>   # one run's outputs
vibe task discard 2 --yes                        # the task, its workspace and its outputs
```

Removing files only affects `--full` and the complete output in the other interfaces:
`vibe trace` then says the output file is missing and shows the preview. Lower
`trace_max_chars` to keep less per call, or set `trace_outputs = false` to stop writing the
store.

## Privacy

Tool outputs can contain secrets: a command that prints an environment variable, a
configuration file an agent read, a token in an error message. The trace store stays on
your machine: `vibe init` puts `tool-output/` in `.vibe/.gitignore`, so it is never
committed, and nothing sends it anywhere.

Two things are worth knowing:

- the event log, `.vibe/tasks/*/events.jsonl`, is committed with the task by default
  (`tasks/` is project history on purpose), and it holds each call's complete **arguments**
  and the 200-character **preview** of its output. Add `tasks/*/events.jsonl` to
  `.vibe/.gitignore` if that is too much to share
  ([What to commit](../design/persistence.md#what-to-commit));
- `vibe serve` returns complete outputs (`GET /api/tasks/{ref}/trace/{call}/output`) to
  anyone holding its token. It listens on the loopback interface by default; with another
  `--bind` address, anyone who can reach it with the token can read them.

## Logs recorded before 0.5

Runs logged by an earlier version have no call ids and no stored outputs. `vibe trace`
pairs their calls with their results by order, per role, tool and subtask, and shows
`paired: by order`; `--full` reports that the output was not traced and shows the preview.

## JSON and other interfaces

`vibe --json trace <REF>` prints the `RunTrace` document (an array of them with `--all`):
`run`, `calls` and `files_written`, each call with its arguments, times, result, preview,
`output_chars` and `output_file`. `--full` does not change it; read `output_file` for the
complete output. The same documents are served by `GET /api/tasks/{ref}/trace` and the
complete output by `GET /api/tasks/{ref}/trace/{call}/output`
([`vibe serve`](cli.md#vibe-serve)). The web UI shows them in the **Trace** tab of a task
(a call expands to its arguments and output; **Load complete output** fetches the whole
text), and `vibe tui` in the **Trace** tab of the task detail (`Enter` expands a call, `o`
loads its complete output).

For what the task delivered in the end (commits, changed files, tokens, cost), see
[History](history.md).
