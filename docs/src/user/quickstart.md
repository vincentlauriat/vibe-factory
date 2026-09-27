# Quick start

This walkthrough takes about ten minutes. It runs one small task on an existing repository,
from `vibe init` to a merged branch. It assumes `vibe` is installed ([Installation](installation.md))
and uses a small Rust command-line project as the example; any language works the same way.

## 1. Choose a model

With an Anthropic key:

```sh
export ANTHROPIC_API_KEY=sk-ant-…
```

Or, with no key at all, a local model through Ollama:

```sh
ollama pull qwen2.5-coder
```

You will point the project at Ollama in step 2. To try the pipeline without any model, jump
to [Try it with no API key](#try-it-with-no-api-key).

## 2. Initialise the project

```sh
cd ~/src/export-tool
git status                  # a clean git repository is the easiest start
vibe init
```

`vibe init` writes `.vibe/config.toml` with every key at its default value and a commented
`[[plugins]]` example, and adds `worktrees/` and `tool-output/` to `.vibe/.gitignore`
(`tasks/` stays committed on purpose). Inspect the effective configuration:

```sh
vibe config show
```

```toml
default_provider = "anthropic"
default_model = "anthropic/claude-sonnet-5"
plugins = []

[phases]

[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.ollama]
kind = "openai"
base_url = "http://localhost:11434/v1"
default_model = "qwen2.5-coder"

[providers.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[pipeline]
max_qa_rounds = 3
max_subtask_attempts = 3
max_parallel_subtasks = 3
max_phase_retries = 2
workspace = "git_worktree"
auto_merge = false
merge_strategy = "manual"

[security]
blocked_commands = []
allowed_commands = []
command_timeout_secs = 120
allow_network = false
extra_read_paths = []
```

Every phase uses `default_model`; `[phases]` is empty until you give a phase its own model.

With Ollama, switch the default model:

```sh
vibe config set default_model ollama/qwen2.5-coder
```

Then check everything is in place:

```sh
vibe doctor
```

Every key is explained in [Configuration](configuration.md).

## 3. Add a task

```sh
vibe task add "Add a --json flag to the export command" \
  -d "When --json is given, print the exported records as a JSON array on stdout instead of the table. Keep the table as the default. Add a test."
```

```text
✓ created task #1 Add a --json flag to the export command
  id   6f1c3e0a-9b2d-4c47-8a51-0e3b8f2d7c19
  dir  /home/me/src/export-tool/.vibe/tasks/001-add-a-json-flag-to-the-export-command

Run it with: vibe run 1
```

The task is stored in `.vibe/tasks/001-add-a-json-flag-to-the-export-command/task.json`, in
the `backlog` status. `vibe task list` shows it.

## 4. Plan without building

A dry run stops after planning, so you can read what the agents intend before they touch any
code:

```sh
vibe run 1 --dry-run
```

It runs three phases:

| Phase | Agent(s) | Produces |
|-------|----------|----------|
| assess | `complexity_assessor` (unless the heuristic already knows) | the complexity and profile, in `task.json` and `run.json` |
| spec | `spec_gatherer`, then `spec_writer` for a standard task | `spec.json` and `spec.md`: summary, requirements with acceptance criteria, relevant files |
| plan | `planner` | `plan.json` and `plan.md`: phases of subtasks with files, dependencies and verification |

The files are in the task directory:

```sh
cd .vibe/tasks/001-add-a-json-flag-to-the-export-command
cat spec.md plan.md progress.md
cd -
```

`progress.md` has one timestamped note per step: the assessment and its source, the spec
summary, the plan. The dry run leaves the run **paused** and the task back in `backlog`, ready to continue;
the exit code is 0 because it stopped where you asked.
If the spec or plan misses the point, edit `spec.json` and run `vibe run 1 --from plan
--dry-run`, or discard the task and add it again with a better description.

## 5. Run it

```sh
vibe run 1 --resume
```

`--resume` continues the paused run at the build phase, reusing the spec and plan you just
read. (`vibe run 1` would start over from the assessment.) The output looks like this:

```text
Resuming #1 Add a --json flag to the export command (model anthropic/claude-sonnet-5, workspace git_worktree)
▶ run 3b1f09c2

── build ──────────
  ▸ subtask 1/3 Add the --json option to the CLI definition: in_progress
  ● coder · subtask 1/3 Add the --json option to the CLI definition
  ⟶ read_file(path: "src/cli.rs")
  ⟵ ok (2 ms)
  ⟶ edit_file(new_string: "    /// Print records as JSON.\n    #[arg(long)]\n    pub json: bool,\n…
  ⟵ ok (4 ms)
  ⟶ bash(command: "cargo build")
  ⟵ ok (8412 ms)
  ● coder finished: 7 step(s), 31.4k in / 1.9k out tokens, completed
  ▸ subtask 1/3 Add the --json option to the CLI definition: done
  ▸ subtask 2/3 Serialise records as JSON in export: in_progress
  ● coder · subtask 2/3 Serialise records as JSON in export
  ⟶ grep(path: "src", pattern: "fn export")
  ⟵ ok (12 ms)
  ⟶ edit_file(new_string: "    if args.json {\n        serde_json::to_writer_pretty(std::io::st…
  ⟵ ok (3 ms)
  ⟶ bash(command: "cargo test export")
  ⟵ error (6120 ms): exit status 101 --- stdout --- running 4 tests test export::tests::json_out…
  ⟶ edit_file(new_string: "#[derive(Serialize)]\npub struct Record {", old_string: "pub stru…
  ⟵ ok (2 ms)
  ⟶ bash(command: "cargo test export")
  ⟵ ok (5810 ms)
  ● coder finished: 12 step(s), 58.0k in / 3.4k out tokens, completed
  ▸ subtask 2/3 Serialise records as JSON in export: done
  ▸ subtask 3/3 Test the --json output: in_progress
  …
  ▸ subtask 3/3 Test the --json output: done
✓ build: 3 done, 0 failed, 0 skipped (4 min 12 s)

── qa ──────────
  ● qa_reviewer
  ⟶ bash(command: "cargo test")
  ⟵ ok (9204 ms)
  ● qa_reviewer finished: 15 step(s), 39.8k in / 1.9k out tokens, completed
✓ qa: round 1: Approved (0 issue(s)) (1 min 03 s)

── merge ──────────
✓ merge: ready for human review and merge (auto_merge is off) (3 ms)
■ run finished: ready

phase  result  time          tokens in  tokens out  summary
build  ✓       4 min 12 s    176.3k     7.9k        3 done, 0 failed, 0 skipped
qa     ✓       1 min 03 s    39.8k      1.9k        round 1: Approved (0 issue(s))
merge  ✓       3 ms          0          0           ready for human review and merge (auto_mer…
status     ready
duration   5 min 16 s
tokens     216.1k in / 9.8k out
branch     vibe/add-a-json-flag-to-the-export-command-6f1c3e0a
worktree   /home/me/src/export-tool/.vibe/worktrees/add-a-json-flag-to-the-export-command-6f1c3e0a
task dir   /home/me/src/export-tool/.vibe/tasks/001-add-a-json-flag-to-the-export-command

Review the work, then merge it: git merge vibe/add-a-json-flag-to-the-export-command-6f1c3e0a
```

A `──` header opens each phase and a `✓` (or `✗`) line closes it with its summary and
duration. `●` lines mark agent sessions, `▸` lines subtask status changes. Each `⟶` line is a
tool call with its arguments (cut at 80 characters) and each `⟵` line its result with the
time it took. A failed tool call (`error`) is normal: the agent reads the error and tries
again. With `-v`, the agents' own text and informational logs are shown too. The final table
sums up the phases of this invocation. `Ctrl-C` cancels; `vibe run 1 --resume` continues later.

Exit code `0` means the task is `ready` (or `done` with auto-merge).

## 6. Inspect the result

```sh
vibe task show 1
```

This prints the spec summary, the plan with each subtask's status, the QA verdict, the
state of the run and the workspace. The code is on the task branch, in its own worktree:

```sh
git log --oneline main..vibe/add-a-json-flag-to-the-export-command-6f1c3e0a
git diff main...vibe/add-a-json-flag-to-the-export-command-6f1c3e0a
cd .vibe/worktrees/add-a-json-flag-to-the-export-command-6f1c3e0a && cargo test && cd -
```

Each completed subtask was committed as `vibe: complete subtask <n> - <title>`.

## 7. Merge

The task is `ready`: nothing reached your branch yet. Merge it as any branch:

```sh
git switch main
git merge vibe/add-a-json-flag-to-the-export-command-6f1c3e0a
vibe task discard 1 --yes          # remove the worktree and the branch
```

To let Vibe Factory merge by itself as soon as QA approves, run with `--auto-merge`, or
set it for the project:

```sh
vibe run 2 --auto-merge
vibe config set pipeline.auto_merge true
```

Auto-merge fast-forwards when it can, otherwise creates a merge commit. It refuses to run
while tracked files of your checkout have uncommitted changes, and it leaves the task
`ready` with the list of conflicting files when the branches conflict. Details in
[Workspaces and merging](workspaces.md#merge-behaviour).

## Try it with no API key

The built-in `mock` provider answers every agent with canned, valid documents without
calling any model. It edits no file, so it is only good for seeing the pipeline move:

```sh
vibe task add "Add a --json flag to the export command"
vibe run 1 --provider mock --dry-run
vibe task show 1
```

This is the CLI's built-in mock, used whenever no `[providers.mock]` table is declared. A
provider you declare yourself with `kind = "mock"` only answers plain text, so its structured
phases fail. The mock assessor classifies the task as `simple`, so the spec is written by the gatherer
alone, and the planner returns a one-subtask plan. Drop `--dry-run` to go through build, QA
and merge as well.

To control what the "model" says, give a script with `--script`. A script is a JSON array of
answers replayed in order, or an object with answers per agent role:

```json
{
  "routes": {
    "planner": [
      {"json": {"approach": "Two steps.", "phases": [{"name": "Work", "parallel": false,
        "subtasks": [{"title": "Add flag", "description": "Add --json."},
                     {"title": "Test it", "description": "Add a test.", "depends_on": ["Add flag"]}]}]}}
    ],
    "qa_reviewer": [
      {"json": {"verdict": "changes_requested", "summary": "Missing test.",
                "issues": [{"severity": "high", "title": "No test for --json"}]}}
    ]
  },
  "fallback": true
}
```

```sh
vibe run 1 --script demo.json      # implies the mock provider on every phase
```

Each request takes the next answer routed to its role, else the next entry of `steps`, else,
with `fallback` true (the default), the canned answer. An answer is a string (plain text),
`{"text": "…"}`, `{"json": …}` (sent as a fenced JSON document, as structured agents
answer), or `{"tool": "read_file", "input": {"path": "src/main.rs"}}` (a tool call, which
really runs). With `"fallback": false`, an exhausted script fails the request. Roles are
recognised from the first line of the agent's system prompt, so routes work for the built-in
prompts; an agent whose prompt you replaced is only served by `steps` and the fallback. The example
above makes QA request changes once, so you can watch the fix loop: the fixer runs, QA
reviews again with the canned approval, and the task ends `ready`.

## Next steps

- [Concepts](concepts.md) explains tasks, profiles, specs, plans and workspaces.
- [Configuration](configuration.md) lists every setting, including models per phase.
- [Troubleshooting](troubleshooting.md) covers what to do when a run pauses or fails.
