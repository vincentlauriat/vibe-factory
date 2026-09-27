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
# workspace = "<name>"       # a WorkspaceProvider registered by an in-process plugin

base_branch = "main"         # top level; default: the branch currently checked out
```

| Value | Isolation | Needs git | Merge |
|-------|-----------|-----------|-------|
| `git_worktree` | one worktree and branch per task | yes | fast-forward, merge commit, or human review |
| `in_place` | none: agents work in the project directory | no | nothing to merge; changes are already there |

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

Merging integrates the task branch into the base branch **in your project checkout**:

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
QA approves; otherwise the task waits in `ready`.

### Assisted conflict resolution

The worktree provider can instead ask a model to resolve conflicts (`MergeStrategy::Assisted`).
For each conflicting file it sends the content with its markers, asks for the fully merged
file, and writes it back only if the answer contains no conflict marker. If every file is
resolved, the merge is committed; if any file is not, the merge is aborted and reported for
human review as above. This strategy is set by the program that builds the workspace
provider; `.vibe/config.toml` has no key for it yet.

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
