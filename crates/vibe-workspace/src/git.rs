//! A small asynchronous wrapper around the `git` command-line tool.
//!
//! Every command runs through [`tokio::process::Command`] with
//! `GIT_TERMINAL_PROMPT=0` (never block on credentials) and `LC_ALL=C`
//! (stable, parseable messages). Commits made by the framework itself use a
//! neutral identity (see [`FRAMEWORK_AUTHOR_NAME`]) so that they succeed on
//! machines without a configured `user.name`.
//!
//! Agents can write anywhere in their worktree and may run `git config`, so
//! the repository configuration is untrusted. Every invocation therefore
//! neutralises the configuration-driven helpers that would execute code in
//! the user's checkout: hooks (`core.hooksPath` points at an empty private
//! directory), the filesystem monitor, the pager, the editor and the SSH
//! command. `GIT_CONFIG_NOSYSTEM=1` also ignores the system-wide config.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::process::Command;
use vibe_core::{Error, Result};

/// Author and committer name used for commits the framework makes.
pub const FRAMEWORK_AUTHOR_NAME: &str = "Vibe Factory";

/// Author and committer e-mail used for commits the framework makes.
pub const FRAMEWORK_AUTHOR_EMAIL: &str = "vibe-factory@localhost";

/// Pathspec excluding the framework's private `.vibe` directory.
pub const EXCLUDE_VIBE: &str = ":(exclude).vibe";

/// Configuration overrides passed as `-c key=value` to every git command,
/// except the read-only configuration queries
/// ([`Git::config_get_unneutralized`], [`Git::config_list_raw`]). `core.hooksPath` is appended
/// separately (see [`empty_hooks_dir`]).
pub const NEUTRAL_CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "core.pager=cat",
    "core.editor=true",
    "core.sshCommand=ssh",
];

/// An empty directory used as `core.hooksPath`, so no hook can run.
///
/// Created once per process under the system temporary directory and kept
/// for the lifetime of the process. `/dev/null` is not used because it is
/// not a path on Windows. If the directory cannot be created, a fresh path
/// that does not exist is used instead: git treats a missing hooks
/// directory as containing no hooks.
#[must_use]
pub fn empty_hooks_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("vibe-no-hooks-")
            .tempdir()
            .map(tempfile::TempDir::keep)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!(
                    "vibe-no-hooks-missing-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_nanos())
                ))
            })
    })
}

/// Raw result of a git invocation that is allowed to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Whether git exited with status 0.
    pub success: bool,
    /// Exit code, if the process was not killed by a signal.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeInfo {
    /// Absolute path of the worktree, as reported by git.
    pub path: PathBuf,
    /// Checked-out commit, if any.
    pub head: Option<String>,
    /// Checked-out branch without the `refs/heads/` prefix.
    pub branch: Option<String>,
    /// Whether this is the bare main repository.
    pub bare: bool,
    /// Whether HEAD is detached.
    pub detached: bool,
    /// Whether the worktree is locked.
    pub locked: bool,
    /// Whether git considers the worktree prunable (directory missing).
    pub prunable: bool,
}

/// Handle on a repository (or worktree) directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    root: PathBuf,
}

impl Git {
    /// Create a handle running every command inside `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Directory the commands run in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn command(&self, args: &[&str], framework_identity: bool, neutral: bool) -> Command {
        let mut cmd = Command::new("git");
        if neutral {
            let mut hooks = std::ffi::OsString::from("core.hooksPath=");
            hooks.push(empty_hooks_dir());
            cmd.arg("-c").arg(hooks);
            for kv in NEUTRAL_CONFIG {
                cmd.arg("-c").arg(kv);
            }
        }
        cmd.args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if framework_identity {
            cmd.env("GIT_AUTHOR_NAME", FRAMEWORK_AUTHOR_NAME)
                .env("GIT_AUTHOR_EMAIL", FRAMEWORK_AUTHOR_EMAIL)
                .env("GIT_COMMITTER_NAME", FRAMEWORK_AUTHOR_NAME)
                .env("GIT_COMMITTER_EMAIL", FRAMEWORK_AUTHOR_EMAIL);
        }
        cmd
    }

    async fn exec(&self, args: &[&str], framework_identity: bool) -> Result<GitOutput> {
        self.exec_with(args, framework_identity, true).await
    }

    async fn exec_with(
        &self,
        args: &[&str],
        framework_identity: bool,
        neutral: bool,
    ) -> Result<GitOutput> {
        tracing::debug!(root = %self.root.display(), args = ?args, neutral, "git");
        let out = self
            .command(args, framework_identity, neutral)
            .output()
            .await
            .map_err(|e| {
                Error::workspace(format!("failed to run git {}: {e}", args.join(" ")))
                    .with_source(e)
            })?;
        let output = GitOutput {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        if !output.success {
            tracing::debug!(code = ?output.code, stderr = %output.stderr.trim(), "git failed");
        }
        Ok(output)
    }

    /// Run git and return its raw output without failing on a non-zero exit.
    pub async fn output(&self, args: &[&str]) -> Result<GitOutput> {
        self.exec(args, false).await
    }

    /// Like [`Git::output`], with the neutral framework identity as author
    /// and committer.
    pub async fn output_as_framework(&self, args: &[&str]) -> Result<GitOutput> {
        self.exec(args, true).await
    }

    /// Run git and return its standard output, or a
    /// [`vibe_core::ErrorKind::Workspace`] error carrying standard error.
    pub async fn run(&self, args: &[&str]) -> Result<String> {
        into_stdout(args, self.exec(args, false).await?)
    }

    /// Like [`Git::run`], with the neutral framework identity as author and
    /// committer.
    pub async fn run_as_framework(&self, args: &[&str]) -> Result<String> {
        into_stdout(args, self.exec(args, true).await?)
    }

    /// Whether the directory is inside a git work tree.
    pub async fn is_repo(&self) -> bool {
        match self.output(&["rev-parse", "--is-inside-work-tree"]).await {
            Ok(out) => out.success && out.stdout.trim() == "true",
            Err(_) => false,
        }
    }

    /// Branch currently checked out, or `None` when HEAD is detached.
    pub async fn current_branch(&self) -> Result<Option<String>> {
        let out = self
            .output(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await?;
        let name = out.stdout.trim();
        Ok((out.success && !name.is_empty()).then(|| name.to_string()))
    }

    /// The branch work should integrate into by default: the current
    /// branch, else `main`, else `master`.
    pub async fn default_branch(&self) -> Result<String> {
        if let Some(branch) = self.current_branch().await? {
            return Ok(branch);
        }
        for candidate in ["main", "master"] {
            if self.branch_exists(candidate).await? {
                return Ok(candidate.to_string());
            }
        }
        Err(Error::workspace(format!(
            "cannot determine a base branch in {}: HEAD is detached and neither main nor master exists",
            self.root.display()
        )))
    }

    /// Whether the working tree has uncommitted changes (tracked or
    /// untracked), ignoring the `.vibe` directory.
    pub async fn has_changes(&self) -> Result<bool> {
        let out = self
            .run(&["status", "--porcelain", "--", ".", EXCLUDE_VIBE])
            .await?;
        Ok(!out.trim().is_empty())
    }

    /// Whether tracked files have uncommitted modifications (staged or
    /// not), ignoring untracked files and the `.vibe` directory.
    pub async fn has_tracked_changes(&self) -> Result<bool> {
        let out = self
            .run(&[
                "status",
                "--porcelain",
                "--untracked-files=no",
                "--",
                ".",
                EXCLUDE_VIBE,
            ])
            .await?;
        Ok(!out.trim().is_empty())
    }

    /// Short status (`git status --short`), ignoring the `.vibe` directory.
    pub async fn status_short(&self) -> Result<String> {
        let out = self
            .run(&["status", "--short", "--", ".", EXCLUDE_VIBE])
            .await?;
        Ok(out.trim_end().to_string())
    }

    /// Full SHA of `HEAD`.
    pub async fn head_sha(&self) -> Result<String> {
        self.rev_parse("HEAD").await
    }

    /// Full SHA of any revision.
    pub async fn rev_parse(&self, rev: &str) -> Result<String> {
        let out = self.run(&["rev-parse", "--verify", rev]).await?;
        Ok(out.trim().to_string())
    }

    /// Whether a local branch with this name exists.
    pub async fn branch_exists(&self, name: &str) -> Result<bool> {
        let reference = format!("refs/heads/{name}");
        let out = self
            .output(&["show-ref", "--verify", "--quiet", &reference])
            .await?;
        Ok(out.success)
    }

    /// Check out an existing branch.
    pub async fn checkout(&self, branch: &str) -> Result<()> {
        self.run(&["checkout", branch]).await.map(|_| ())
    }

    /// Registered worktrees, parsed from `git worktree list --porcelain`.
    pub async fn worktree_list(&self) -> Result<Vec<WorktreeInfo>> {
        let out = self.run(&["worktree", "list", "--porcelain"]).await?;
        Ok(parse_worktree_list(&out))
    }

    /// Remove administrative data of worktrees whose directory vanished.
    pub async fn worktree_prune(&self) -> Result<()> {
        self.run(&["worktree", "prune"]).await.map(|_| ())
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
    pub async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let out = self
            .output(&[
                "merge-base",
                "--is-ancestor",
                "--end-of-options",
                ancestor,
                descendant,
            ])
            .await?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::workspace(format!(
                "git merge-base --is-ancestor {ancestor} {descendant}: {}",
                out.stderr.trim()
            ))),
        }
    }

    /// Number of commits reachable from `head` but not from `base`.
    pub async fn commits_ahead(&self, base: &str, head: &str) -> Result<u64> {
        let range = format!("{base}..{head}");
        let out = self.run(&["rev-list", "--count", &range]).await?;
        out.trim().parse().map_err(|_| {
            Error::workspace(format!(
                "unexpected output from git rev-list --count: {out:?}"
            ))
        })
    }

    /// `git diff --stat base...head` (changes since the merge base).
    pub async fn diff_stat(&self, base: &str, head: &str) -> Result<String> {
        let range = format!("{base}...{head}");
        let out = self
            .run(&["diff", "--stat", &range, "--", ".", EXCLUDE_VIBE])
            .await?;
        Ok(out.trim_end().to_string())
    }

    /// `git diff base...head` (full patch since the merge base).
    pub async fn diff(&self, base: &str, head: &str) -> Result<String> {
        let range = format!("{base}...{head}");
        self.run(&["diff", &range, "--", ".", EXCLUDE_VIBE]).await
    }

    /// Files left with unresolved conflicts by a failed merge.
    pub async fn conflicted_files(&self) -> Result<Vec<String>> {
        let out = self
            .run(&["diff", "--name-only", "--diff-filter=U"])
            .await?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Read a repository-local configuration value.
    pub async fn config_get(&self, key: &str) -> Result<Option<String>> {
        let out = self.output(&["config", "--local", "--get", key]).await?;
        let value = out.stdout.trim();
        Ok((out.success && !value.is_empty()).then(|| value.to_string()))
    }

    /// Effective value of `key` as the repository itself configures it,
    /// read without the framework's `-c` overrides (which would otherwise
    /// mask it). Reading configuration runs no hook or helper.
    pub async fn config_get_unneutralized(&self, key: &str) -> Result<Option<String>> {
        let out = self
            .exec_with(&["config", "--get", key], false, false)
            .await?;
        let value = out.stdout.trim();
        Ok((out.success && !value.is_empty()).then(|| value.to_string()))
    }

    /// Raw `git config <scope> --list -z` output (NUL-separated entries,
    /// each `key` or `key\nvalue`), read without the framework's `-c`
    /// overrides. `scope` is a flag such as `--local` or `--worktree`.
    /// Listing configuration runs no hook or helper.
    pub async fn config_list_raw(&self, scope: &str) -> Result<String> {
        let args = ["config", scope, "--list", "-z"];
        into_stdout(&args, self.exec_with(&args, false, false).await?)
    }

    /// Write a repository-local configuration value.
    pub async fn config_set(&self, key: &str, value: &str) -> Result<()> {
        self.run(&["config", "--local", key, value])
            .await
            .map(|_| ())
    }
}

fn into_stdout(args: &[&str], out: GitOutput) -> Result<String> {
    if out.success {
        return Ok(out.stdout);
    }
    let detail = if out.stderr.trim().is_empty() {
        out.stdout.trim()
    } else {
        out.stderr.trim()
    };
    let code = out
        .code
        .map_or_else(|| "signal".to_string(), |c| c.to_string());
    Err(Error::workspace(format!(
        "git {} failed (exit {code}): {detail}",
        args.join(" ")
    )))
}

/// Parse the output of `git worktree list --porcelain`.
#[must_use]
pub fn parse_worktree_list(porcelain: &str) -> Vec<WorktreeInfo> {
    let mut list = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    for line in porcelain.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            list.extend(current.take());
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            list.extend(current.take());
            current = Some(WorktreeInfo {
                path: PathBuf::from(value),
                ..WorktreeInfo::default()
            });
            continue;
        }
        let Some(info) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => info.head = Some(value.to_string()),
            "branch" => {
                info.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_string(),
                );
            }
            "bare" => info.bare = true,
            "detached" => info.detached = true,
            "locked" => info.locked = true,
            "prunable" => info.prunable = true,
            _ => {}
        }
    }
    list.extend(current);
    list
}

/// Whether two paths designate the same directory, resolving symlinks and
/// platform-specific prefixes when both exist.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_porcelain_worktree_list() {
        let text = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                    worktree /repo/.vibe/worktrees/x\nHEAD def\nbranch refs/heads/vibe/x\nlocked\n\n\
                    worktree /gone\nHEAD 123\ndetached\nprunable gitdir file points to non-existent location\n";
        let list = parse_worktree_list(text);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[1].path, PathBuf::from("/repo/.vibe/worktrees/x"));
        assert_eq!(list[1].branch.as_deref(), Some("vibe/x"));
        assert!(list[1].locked);
        assert!(list[2].detached && list[2].prunable);
        assert_eq!(list[2].branch, None);
    }

    #[test]
    fn failure_message_contains_stderr() {
        let err = into_stdout(
            &["merge", "x"],
            GitOutput {
                success: false,
                code: Some(1),
                stdout: String::new(),
                stderr: "fatal: boom\n".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Workspace);
        assert!(
            err.message
                .contains("git merge x failed (exit 1): fatal: boom")
        );
    }
}
