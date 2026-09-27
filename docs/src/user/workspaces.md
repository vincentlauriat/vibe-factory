# Workspaces and merging

Agents never edit your checkout by default. Each task gets its own **workspace**: a git
worktree on a dedicated branch. The agents are confined to that directory, you keep working in
your checkout, and several tasks can run side by side. When the work is approved, the branch
is merged into the base branch, or left for you to review. The design rationale is in
[ADR-003](../design/adr/003-git-worktrees.md).

## Choosing a workspace

```toml
[pipeline]
workspace = "git_worktree"   # default
# workspace = "in_place"     # no isolation
# workspace = "container"    # git worktrees, shell commands in containers
# workspace = "<name>"       # a WorkspaceProvider registered by an in-process plugin

base_branch = "main"         # top level; default: the branch currently checked out
```

| Value | Isolation | Needs git | Merge |
|-------|-----------|-----------|-------|
| `git_worktree` | one worktree and branch per task | yes | fast-forward, merge commit, or human review |
| `in_place` | none: agents work in the project directory | no | nothing to merge; changes are already there |
| `container` | git worktrees as above, and every shell command in a throw-away container | yes, and Docker or Podman | as `git_worktree` |

## Git worktrees

For a task titled "Add OAuth login (GitHub)" with id `6f1c3e0a-…`, the workspace is:

| What | Value |
|------|-------|
| name | `<slug>-<first 8 characters of the task id>`, for example `add-oauth-login-github-6f1c3e0a` |
| directory | `<project>/.vibe/worktrees/add-oauth-login-github-6f1c3e0a` |
| branch | `vibe/add-oauth-login-github-6f1c3e0a` |
| forked from | `base_branch`, else the branch currently checked out, else `main`, else `master` |

The slug is the title in lowercase, non-alphanumeric runs replaced by `-`, at most 48
characters. The directory `.vibe/worktrees/` contains a `.gitignore` with `*`, so worktrees
never appear in your own `git status`.

The base branch is recorded in the repository configuration under
`branch.vibe/<name>.vibebase`, so a task always merges back into the branch it was forked
from, even if you change `base_branch` later:

```sh
git config --get branch.vibe/add-oauth-login-github-6f1c3e0a.vibebase
```

**Reopening.** Running a task again reuses its worktree. Before that, `git worktree prune`
forgets worktrees whose directory you deleted. If the directory exists but git does not know
it (a leftover), it is removed and recreated. If the branch exists but its worktree is gone,
a new worktree is created on the existing branch, keeping its commits.

The project must be a git repository; otherwise opening fails with
``<path> is not a git repository; use the in_place workspace or run `git init` ``.

### One worktree per subtask attempt

With `git_worktree` (and `pipeline.isolate_subtasks = true`, the default), every attempt at a
subtask also gets its own worktree, forked from the current commit of the task branch:

| What | Value |
|------|-------|
| directory | `<task worktree>--s<N>-a<attempt>`, for example `.vibe/worktrees/add-oauth-login-github-6f1c3e0a--s2-a1` |
| branch | `<task branch>--s<N>-a<attempt>`, for example `vibe/add-oauth-login-github-6f1c3e0a--s2-a1` |

Parallel coder sessions therefore never see each other's unfinished edits, and a failed
attempt is thrown away with its worktree instead of being reset in a shared directory.

When a session reports its subtask done, its work is committed on the attempt branch
(`vibe: complete subtask 2 - Routes`) and merged into the task branch right away: a
fast-forward when the task branch did not move, else a merge commit. Integrations happen one
at a time; sessions that finish together are integrated in plan order, and a subtask starts
only once the work of its `depends_on` subtasks is integrated. If the merge conflicts with
work integrated since the attempt started, the merge is aborted (the task branch is left
exactly as it was), the attempt counts as failed with the conflicting files in its notes,
and the next attempt starts from the updated task branch. With `merge_strategy =
"assisted"`, the model of the merge phase first tries to resolve the conflict markers, as for
the final merge; the attempt only fails when a file stays unresolved.

Attempt worktrees and branches are removed after each attempt and, to clean up after a
crash, when the build starts and ends. `vibe task discard` removes any that remain. Before
the first attempt the task worktree is committed as `vibe: checkpoint before build`, so
attempts fork from everything already in it.

Set `pipeline.isolate_subtasks = false` to have parallel subtasks share the task worktree as
in 0.1 (completed subtasks are then committed when no other session is running, and failed
attempts are reset). `in_place` and plugin workspaces always share the task workspace.

## Container workspace

The shell policy filters commands but is not a sandbox. With `workspace = "container"`, the
`bash` tool, and therefore the required validation commands, runs every command in a fresh
container instead of on your machine:

```toml
[pipeline]
workspace = "container"

[workspace.container]
image = "rust:1.88"          # required
runtime = "docker"           # or "podman", or a path to either
network = "none"             # default; "bridge" or a named network
cpus = 2.0
memory = "4g"                # also the swap limit
pids_limit = 1024            # default
tmp_size = "1g"              # size of the /tmp tmpfs
# user = "1000:1000"         # default: owner of the worktree on Unix
env = ["CARGO_TERM_COLOR"]   # host variables passed through, by name only
mount_git_metadata = false   # mount the git directory read-only (Unix only)

[[workspace.container.mounts]]
source = "/home/me/.cargo/registry" # absolute, or relative to the project root; no `~`
target = "/usr/local/cargo/registry"
read_only = true             # default
```

Files still live in the task's git worktree on the host: it is bind-mounted at `/workspace`
and the command's working directory is translated to the matching path inside the
container. The file tools, the diff shown to reviewers, commits, merges and validated merges
work exactly as with `git_worktree`, and so do subtask worktrees.

Every container is started with `--rm`, `--cap-drop=ALL`,
`--security-opt=no-new-privileges`, a read-only root file system, a `/tmp` tmpfs, a process
limit and a label `io.vibe-factory.managed=true`. There is no way to pass raw flags to the
runtime; unknown keys in `[workspace.container]` are an error. One container runs per command
and is removed with `rm --force` when the command ends, times out or is cancelled, which also
kills its background processes. The start-up costs a few hundred milliseconds per command.

What the settings allow and refuse:

* **Network.** `none` unless the configured network is `bridge` or a named network **and**
  `security.allow_network = true`. `host` and `container:<id>` are refused.
* **Mounts.** The source must exist; the filesystem root and sockets (such as the Docker
  socket) are refused; a writable mount may not overlap the project's `.git` or `.vibe`;
  targets must be absolute and outside `/workspace`, `/tmp`, `/proc`, `/sys` and `/dev`.
* **Git.** The worktree's `.git` pointer is mounted read-only, and the repository's git
  directory is not mounted unless `mount_git_metadata = true` (then read-only), so git
  commands that write do not work inside the container; the framework commits on the host.
* **Failures of the runtime.** An exit status of 125 (image missing, daemon down) is reported
  to the agent as a runner failure with `sandbox_error: true` in the tool metadata.

The settings are validated before a run starts. `vibe doctor` also checks that `<runtime>
info` answers. The image must already contain the tools the agents need (compilers, test
runners); nothing is installed for you.

## In-place mode

`pipeline.workspace = "in_place"` makes the project directory itself the workspace. Use it
for directories that are not git repositories, throw-away experiments, or when you want to
watch files change live. There is no branch, merging does nothing (`NoChanges`), and
discarding a task leaves your files untouched. Agents still cannot leave the project
directory.

## What changed

The workspace summary shown to reviewers (and to you) has two parts, both excluding
`.vibe/`:

```text
Committed changes since main:
 src/auth/github.rs | 84 +++++++++++++++++++++++
 src/routes.rs      | 12 +++-
 2 files changed, 94 insertions(+), 2 deletions(-)

Uncommitted changes:
 M src/routes.rs
?? tests/auth_github.rs
```

The first part is `git diff --stat <base>...HEAD` (changes since the fork point); the second
is `git status --short` in the worktree.

## Merge behaviour

With configured `pipeline.validation_commands` and `auto_merge = true`, integration is
prepared in a detached temporary worktree. The commands run against the combined result,
including assisted conflict resolutions. Only a passing, unchanged candidate is published
by fast-forwarding the base branch to that exact commit. A failed check, unresolved conflict,
changed target or unsupported provider pauses in `review`; the target is not updated by the
integration. Resume builds and checks a fresh candidate. See
[Required validation commands](configuration.md#required-validation-commands).

Without configured validation commands, merging integrates the task branch into the base
branch **in your project checkout** using the original algorithm:

1. Anything left uncommitted in the worktree is committed as `vibe: checkpoint`, excluding
   `.vibe/`, with the neutral identity `Vibe Factory <vibe-factory@localhost>`.
2. If tracked files of your checkout have uncommitted modifications, the merge is refused:
   `cannot merge vibe/… : the project at … has uncommitted changes; commit or stash them
   first`. Untracked files do not block it.
3. If the task branch has no commit ahead of the base, the result is **no changes**.
4. Your checkout is switched to the base branch if needed, then `git merge --ff-only` is
   tried; if the base moved, `git merge --no-ff` creates a commit `Merge vibe/… (vibe)`.
5. On conflicts, the merge is aborted, your checkout is switched back to the branch it was
   on, and the task is handed to you for **human review** with the list of conflicting
   files. Your checkout is left clean.

After a successful merge your checkout stays on the base branch. The worktree and branch are
kept until you discard the task. With `pipeline.auto_merge = true` the merge runs as soon as
QA and required validations approve; otherwise the task waits in `ready` after its task checks.

### Assisted conflict resolution

The worktree provider can instead ask a model to resolve conflicts (`MergeStrategy::Assisted`).
For each conflicting file it sends the content with its markers, asks for the fully merged
file, and writes it back only if the answer contains no conflict marker. If every file is
resolved, the merge is committed; if any file is not, the merge is aborted and reported for
human review as above. Enable it with `pipeline.merge_strategy = "assisted"` in
`.vibe/config.toml`; the model used is the one configured for the `merge` phase (or
`default_model`).

## Repository configuration is guarded

Agents may run `git` in the worktree, and a worktree shares the repository's local
configuration (`.git/config`) with your checkout. To make sure an agent cannot smuggle
executable settings into your repository, the worktree provider:

- runs every git command of the framework with hooks, `fsmonitor`, pagers, editors and
  SSH helpers neutralised (`-c core.hooksPath=<empty dir>` and friends);
- takes a snapshot of the local configuration when the workspace is opened (kept in memory
  and in `.vibe/worktrees/.snapshots/<task>.cfg`);
- refuses to merge when any key changed since, except the framework's own `branch.vibe/*`
  sections. The error lists the added, removed or modified keys, with `core.hooksPath`
  named first when it is involved.

If you changed the configuration yourself in the meantime, review the listed keys and
either restore them or accept them (`GitWorktreeProvider::accept_config_changes` from code;
a CLI command is on the roadmap), then run the merge again.

## Inspecting, merging and discarding by hand

Everything is plain git, so you can take over at any point:

```sh
git worktree list                                   # every task workspace
git log --oneline main..vibe/add-oauth-login-github-6f1c3e0a
git diff main...vibe/add-oauth-login-github-6f1c3e0a

cd .vibe/worktrees/add-oauth-login-github-6f1c3e0a  # run tests, edit, commit
cargo test
git commit -am "fix: handle missing email scope"

cd -                                                # back to your checkout
git switch main
git merge vibe/add-oauth-login-github-6f1c3e0a      # resolve conflicts as usual
```

To throw a task's work away:

```sh
vibe task discard <task>                            # removes worktree and branch
# or by hand:
git worktree remove --force .vibe/worktrees/add-oauth-login-github-6f1c3e0a
git branch -D vibe/add-oauth-login-github-6f1c3e0a
git worktree prune
```

Discarding is idempotent: a worktree or branch that is already gone is not an error. The
framework refuses to discard a workspace whose root is the project itself.

## Disk usage

Each worktree is a full checkout of your tracked files (the object database is shared, so
history is not duplicated), plus whatever the agents build inside it: `target/`,
`node_modules/`, virtual environments. On large projects, budget one working copy and one
build directory per open task. Truncated tool outputs are also saved under
`.vibe/tool-output/` inside the workspace. Discard finished tasks to reclaim the space, and
check with:

```sh
du -sh .vibe/worktrees/*
```
