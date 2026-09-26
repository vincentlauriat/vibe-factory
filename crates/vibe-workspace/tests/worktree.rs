//! Integration tests against real temporary git repositories.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, Message, ModelProvider, Result, StopReason, Task, Usage,
    WorkspaceKind, WorkspaceProvider,
};
use vibe_workspace::git::{FRAMEWORK_AUTHOR_NAME, same_path};
use vibe_workspace::{
    Git, GitWorktreeProvider, MergeOutcome, MergeStrategy, commit_all, has_uncommitted,
    provider_by_name,
};

/// A temporary repository with one initial commit.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// Branch created by `git init` (`main` or `master` depending on git).
    base: String,
}

/// Run git synchronously for test setup, independent of the user's config.
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        // Repository-local settings override whatever the machine has
        // globally, so the framework's own git calls behave predictably.
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir(&hooks).unwrap();
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("merge.ff", "true"),
            ("core.hooksPath", hooks.to_str().unwrap()),
        ] {
            git(&root, &["config", k, v]);
        }
        std::fs::write(root.join("a.txt"), "line 1\nline 2\nline 3\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        let base = git(&root, &["symbolic-ref", "--short", "HEAD"]);
        Self {
            _dir: dir,
            root,
            base,
        }
    }

    fn commit_file(&self, dir: &Path, file: &str, content: &str, msg: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", msg]);
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.root.join(file)).unwrap()
    }

    fn branch_exists(&self, branch: &str) -> bool {
        Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(&self.root)
            .status()
            .unwrap()
            .success()
    }
}

fn provider() -> GitWorktreeProvider {
    GitWorktreeProvider::new()
}

#[tokio::test]
async fn open_creates_isolated_worktree() {
    let repo = Repo::new();
    let task = Task::new("Add login page", "");
    let ws = provider().open(&repo.root, &task).await.unwrap();

    let name = format!("add-login-page-{}", task.id.short());
    assert_eq!(ws.kind, WorkspaceKind::GitWorktree);
    assert_eq!(ws.project_root, repo.root);
    assert_eq!(
        ws.root,
        repo.root.join(".vibe").join("worktrees").join(&name)
    );
    assert_eq!(ws.branch.as_deref(), Some(format!("vibe/{name}").as_str()));
    assert_eq!(ws.base_branch.as_deref(), Some(repo.base.as_str()));
    assert!(ws.root.join("a.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(repo.root.join(".vibe/worktrees/.gitignore")).unwrap(),
        "*\n"
    );
    // The worktree is invisible to the project's status.
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&ws.root, &["symbolic-ref", "--short", "HEAD"]),
        format!("vibe/{name}")
    );

    let listed = Git::new(&repo.root).worktree_list().await.unwrap();
    assert!(listed.iter().any(|w| same_path(&w.path, &ws.root)));
}

#[tokio::test]
async fn reopen_reuses_registered_worktree() {
    let repo = Repo::new();
    let task = Task::new("Reuse", "");
    let ws = provider().open(&repo.root, &task).await.unwrap();
    std::fs::write(ws.root.join("wip.txt"), "in progress\n").unwrap();

    let again = provider().open(&repo.root, &task).await.unwrap();
    assert_eq!(again, ws);
    assert!(again.root.join("wip.txt").is_file(), "work is preserved");
}

#[tokio::test]
async fn reopen_recreates_missing_or_stale_directory() {
    let repo = Repo::new();
    let task = Task::new("Stale", "");
    let ws = provider().open(&repo.root, &task).await.unwrap();
    repo.commit_file(&ws.root, "c.txt", "c\n", "add c");

    // Directory deleted behind git's back: prune + re-add on the existing branch.
    std::fs::remove_dir_all(&ws.root).unwrap();
    let again = provider().open(&repo.root, &task).await.unwrap();
    assert_eq!(again.root, ws.root);
    assert!(again.root.join("c.txt").is_file(), "branch commits survive");

    // Unregistered directory at the target path: removed and recreated.
    let task2 = Task::new("Leftover", "");
    let path = repo
        .root
        .join(".vibe")
        .join("worktrees")
        .join(GitWorktreeProvider::task_name(&task2));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("junk"), "x").unwrap();
    let ws2 = provider().open(&repo.root, &task2).await.unwrap();
    assert!(!ws2.root.join("junk").exists());
    assert!(ws2.root.join("a.txt").is_file());
}

#[tokio::test]
async fn open_rejects_non_repository() {
    let dir = tempfile::tempdir().unwrap();
    let err = provider()
        .open(dir.path(), &Task::new("x", ""))
        .await
        .unwrap_err();
    assert_eq!(err.kind, vibe_core::ErrorKind::Workspace);
    assert!(err.message.contains("not a git repository"));
}

#[tokio::test]
async fn changes_reports_committed_and_uncommitted_work() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Changes", ""))
        .await
        .unwrap();
    assert_eq!(provider().changes(&ws).await.unwrap(), "");

    repo.commit_file(&ws.root, "new.txt", "hello\n", "add new");
    std::fs::write(ws.root.join("b.txt"), "b changed\n").unwrap();
    std::fs::write(ws.root.join("untracked.txt"), "u\n").unwrap();

    let summary = provider().changes(&ws).await.unwrap();
    assert!(summary.contains(&format!("Committed changes since {}", repo.base)));
    assert!(summary.contains("new.txt"));
    assert!(summary.contains("Uncommitted changes:"));
    assert!(summary.contains("b.txt"));
    assert!(summary.contains("untracked.txt"));
}

#[tokio::test]
async fn merge_fast_forwards_and_checkpoints_uncommitted_work() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Fast forward", ""))
        .await
        .unwrap();
    std::fs::write(ws.root.join("feature.txt"), "feature\n").unwrap();

    let outcome = provider().merge(&ws).await.unwrap();
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    assert_eq!(
        outcome,
        MergeOutcome::Merged {
            commit: Some(head.clone())
        }
    );
    assert_eq!(repo.read("feature.txt"), "feature\n");
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%s"]),
        "vibe: checkpoint"
    );
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%an"]),
        FRAMEWORK_AUTHOR_NAME
    );
    // Fast-forward: a single parent.
    assert_eq!(
        git(&repo.root, &["rev-list", "--parents", "-1", "HEAD"])
            .split(' ')
            .count(),
        2
    );
    assert_eq!(
        git(&repo.root, &["symbolic-ref", "--short", "HEAD"]),
        repo.base
    );
}

#[tokio::test]
async fn merge_creates_merge_commit_when_base_moved() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("True merge", ""))
        .await
        .unwrap();
    repo.commit_file(&ws.root, "feature.txt", "feature\n", "feature");
    repo.commit_file(&repo.root, "b.txt", "b on base\n", "base moved");

    let outcome = provider().merge(&ws).await.unwrap();
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    assert_eq!(outcome, MergeOutcome::Merged { commit: Some(head) });
    assert_eq!(
        git(&repo.root, &["rev-list", "--parents", "-1", "HEAD"])
            .split(' ')
            .count(),
        3,
        "merge commit has two parents"
    );
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%s"]),
        format!("Merge {} (vibe)", ws.branch.as_deref().unwrap())
    );
    assert_eq!(repo.read("feature.txt"), "feature\n");
    assert_eq!(repo.read("b.txt"), "b on base\n");
}

#[tokio::test]
async fn merge_conflict_needs_human_review_and_leaves_project_clean() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Conflict", ""))
        .await
        .unwrap();
    repo.commit_file(
        &ws.root,
        "a.txt",
        "line 1\nfrom task\nline 3\n",
        "task edit",
    );
    repo.commit_file(
        &repo.root,
        "a.txt",
        "line 1\nfrom base\nline 3\n",
        "base edit",
    );
    let before = git(&repo.root, &["rev-parse", "HEAD"]);

    let outcome = provider().merge(&ws).await.unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::NeedsHumanReview {
            files: vec!["a.txt".to_string()]
        }
    );
    assert_eq!(git(&repo.root, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    assert_eq!(repo.read("a.txt"), "line 1\nfrom base\nline 3\n");
}

#[tokio::test]
async fn merge_conflict_restores_the_branch_the_project_was_on() {
    let repo = Repo::new();
    let p = provider().with_base_branch(Some(repo.base.clone()));
    let ws = p.open(&repo.root, &Task::new("Restore", "")).await.unwrap();
    repo.commit_file(&ws.root, "a.txt", "task\n", "task edit");
    repo.commit_file(&repo.root, "a.txt", "base\n", "base edit");
    git(&repo.root, &["checkout", "--quiet", "-b", "human-work"]);

    let outcome = p.merge(&ws).await.unwrap();
    assert!(matches!(outcome, MergeOutcome::NeedsHumanReview { .. }));
    assert_eq!(
        git(&repo.root, &["symbolic-ref", "--short", "HEAD"]),
        "human-work"
    );
}

#[tokio::test]
async fn merge_without_changes_reports_no_changes() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Nothing", ""))
        .await
        .unwrap();
    assert_eq!(
        provider().merge(&ws).await.unwrap(),
        MergeOutcome::NoChanges
    );
}

#[tokio::test]
async fn merge_refuses_dirty_project() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Dirty", ""))
        .await
        .unwrap();
    repo.commit_file(&ws.root, "feature.txt", "f\n", "feature");
    std::fs::write(repo.root.join("b.txt"), "human edit\n").unwrap();

    let err = provider().merge(&ws).await.unwrap_err();
    assert_eq!(err.kind, vibe_core::ErrorKind::Workspace);
    assert!(
        err.message.contains("uncommitted changes"),
        "{}",
        err.message
    );
    assert_eq!(repo.read("b.txt"), "human edit\n");
}

#[tokio::test]
async fn discard_removes_worktree_and_branch_idempotently() {
    let repo = Repo::new();
    let ws = provider()
        .open(&repo.root, &Task::new("Throw away", ""))
        .await
        .unwrap();
    repo.commit_file(&ws.root, "x.txt", "x\n", "x");
    let branch = ws.branch.clone().unwrap();
    assert!(repo.branch_exists(&branch));

    provider().discard(&ws).await.unwrap();
    assert!(!ws.root.exists());
    assert!(!repo.branch_exists(&branch));
    let listed = Git::new(&repo.root).worktree_list().await.unwrap();
    assert_eq!(listed.len(), 1, "only the main worktree remains");

    provider().discard(&ws).await.unwrap();
}

struct ResolvingProvider;

#[async_trait::async_trait]
impl ModelProvider for ResolvingProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "resolver".into(),
            supports_tools: false,
            supports_thinking: false,
            default_model: "m".into(),
        }
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            message: Message::assistant("line 1\nfrom base and task\nline 3\n"),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: "m".into(),
        })
    }
}

#[tokio::test]
async fn assisted_merge_commits_model_resolution() {
    let repo = Repo::new();
    let p = provider().with_merge_strategy(MergeStrategy::Assisted {
        provider: Arc::new(ResolvingProvider),
        model: "m".into(),
    });
    let ws = p
        .open(&repo.root, &Task::new("Assisted", ""))
        .await
        .unwrap();
    repo.commit_file(
        &ws.root,
        "a.txt",
        "line 1\nfrom task\nline 3\n",
        "task edit",
    );
    repo.commit_file(
        &repo.root,
        "a.txt",
        "line 1\nfrom base\nline 3\n",
        "base edit",
    );

    let outcome = p.merge(&ws).await.unwrap();
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    assert_eq!(outcome, MergeOutcome::Merged { commit: Some(head) });
    assert_eq!(repo.read("a.txt"), "line 1\nfrom base and task\nline 3\n");
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&repo.root, &["rev-list", "--parents", "-1", "HEAD"])
            .split(' ')
            .count(),
        3
    );
}

#[tokio::test]
async fn commit_all_excludes_vibe_and_skips_empty_commits() {
    let repo = Repo::new();
    assert_eq!(commit_all(&repo.root, "nothing", &[]).await.unwrap(), None);

    std::fs::create_dir_all(repo.root.join(".vibe").join("tasks")).unwrap();
    std::fs::write(repo.root.join(".vibe/tasks/state.json"), "{}").unwrap();
    std::fs::write(repo.root.join("secret.env"), "k=v\n").unwrap();
    std::fs::write(repo.root.join("code.rs"), "fn main() {}\n").unwrap();
    assert!(has_uncommitted(&repo.root).await.unwrap());

    let sha = commit_all(&repo.root, "work", &["secret.env"])
        .await
        .unwrap()
        .expect("a commit is made");
    assert_eq!(git(&repo.root, &["rev-parse", "HEAD"]), sha);
    let files = git(&repo.root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files, "code.rs");
    assert_eq!(
        commit_all(&repo.root, "again", &["secret.env"])
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn git_helper_queries() {
    let repo = Repo::new();
    let g = Git::new(&repo.root);
    assert!(g.is_repo().await);
    assert_eq!(g.current_branch().await.unwrap(), Some(repo.base.clone()));
    assert_eq!(g.default_branch().await.unwrap(), repo.base);
    assert!(g.branch_exists(&repo.base).await.unwrap());
    assert!(!g.branch_exists("nope").await.unwrap());
    assert_eq!(g.head_sha().await.unwrap().len(), 40);
    assert!(!g.has_changes().await.unwrap());

    // Detached HEAD falls back to the existing main/master branch.
    git(&repo.root, &["checkout", "--quiet", "--detach"]);
    assert_eq!(g.current_branch().await.unwrap(), None);
    assert_eq!(g.default_branch().await.unwrap(), repo.base);

    let err = g
        .run(&["rev-parse", "--verify", "no-such-ref"])
        .await
        .unwrap_err();
    assert_eq!(err.kind, vibe_core::ErrorKind::Workspace);
    assert!(!Git::new(repo.root.parent().unwrap()).is_repo().await);
}

#[tokio::test]
async fn provider_by_name_builds_working_providers() {
    let repo = Repo::new();
    let task = Task::new("By name", "");
    let wt = provider_by_name("git_worktree", Some(repo.base.clone())).unwrap();
    let ws = wt.open(&repo.root, &task).await.unwrap();
    assert_eq!(ws.base_branch.as_deref(), Some(repo.base.as_str()));
    wt.discard(&ws).await.unwrap();

    let ip = provider_by_name("in_place", None).unwrap();
    let ws = ip.open(&repo.root, &task).await.unwrap();
    assert_eq!(ws.root, repo.root);
}
