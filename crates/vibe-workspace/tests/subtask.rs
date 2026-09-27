//! Subtask worktrees against real temporary git repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use vibe_core::{SubtaskIntegration, SubtaskWorkspaces, Task, Workspace, WorkspaceProvider};
use vibe_workspace::{GitSubtaskWorkspaces, GitWorktreeProvider};

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

struct Fixture {
    _dir: tempfile::TempDir,
    task_ws: Workspace,
}

impl Fixture {
    /// A repository with `a.txt` (three lines) and `b.txt`, and the git
    /// worktree workspace of one task.
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root: PathBuf = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir(&hooks).unwrap();
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", hooks.to_str().unwrap()),
        ] {
            git(&root, &["config", k, v]);
        }
        std::fs::write(root.join("a.txt"), "line 1\nline 2\nline 3\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        let task = Task::new("Parallel work", "two subtasks");
        let task_ws = GitWorktreeProvider::new().open(&root, &task).await.unwrap();
        Self { _dir: dir, task_ws }
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.task_ws.root.join(file)).unwrap()
    }

    fn task_status(&self) -> String {
        git(&self.task_ws.root, &["status", "--porcelain"])
    }
}

#[tokio::test]
async fn parallel_attempts_are_isolated_and_integrated_in_order() {
    let f = Fixture::new().await;
    let subs = GitSubtaskWorkspaces::new();
    let one = subs.open(&f.task_ws, "s1-a1").await.unwrap();
    let two = subs.open(&f.task_ws, "s2-a1").await.unwrap();
    assert_ne!(one.root, two.root);
    assert_eq!(one.base_branch, f.task_ws.branch);

    std::fs::write(one.root.join("c.txt"), "from one\n").unwrap();
    std::fs::write(two.root.join("b.txt"), "b changed by two\n").unwrap();
    // Neither attempt sees the other's edits.
    assert!(!two.root.join("c.txt").exists());
    assert_eq!(
        std::fs::read_to_string(one.root.join("b.txt")).unwrap(),
        "b\n"
    );

    let first = subs.integrate(&f.task_ws, &one, "subtask 1").await.unwrap();
    assert!(matches!(
        first,
        SubtaskIntegration::Integrated { commit: Some(_) }
    ));
    let second = subs.integrate(&f.task_ws, &two, "subtask 2").await.unwrap();
    assert!(matches!(
        second,
        SubtaskIntegration::Integrated { commit: Some(_) }
    ));
    assert_eq!(f.read("c.txt"), "from one\n");
    assert_eq!(f.read("b.txt"), "b changed by two\n");
    assert_eq!(f.task_status(), "");
    let log = git(&f.task_ws.root, &["log", "--format=%s"]);
    assert!(log.starts_with("subtask 2\n"), "{log}");
    assert!(log.contains("subtask 1"));

    subs.discard(&one).await.unwrap();
    subs.discard(&two).await.unwrap();
    assert!(!one.root.exists() && !two.root.exists());
    let branches = git(&f.task_ws.root, &["branch", "--list", "*--s*"]);
    assert_eq!(branches, "");
    // Discarding twice is not an error.
    subs.discard(&one).await.unwrap();
}

#[tokio::test]
async fn conflicting_attempt_leaves_the_task_untouched_and_retry_succeeds() {
    let f = Fixture::new().await;
    let subs = GitSubtaskWorkspaces::new();
    let one = subs.open(&f.task_ws, "s1-a1").await.unwrap();
    let two = subs.open(&f.task_ws, "s2-a1").await.unwrap();
    std::fs::write(one.root.join("a.txt"), "line 1\nONE\nline 3\n").unwrap();
    std::fs::write(two.root.join("a.txt"), "line 1\nTWO\nline 3\n").unwrap();

    let first = subs.integrate(&f.task_ws, &one, "subtask 1").await.unwrap();
    assert!(matches!(first, SubtaskIntegration::Integrated { .. }));
    let head = git(&f.task_ws.root, &["rev-parse", "HEAD"]);
    let second = subs.integrate(&f.task_ws, &two, "subtask 2").await.unwrap();
    assert_eq!(
        second,
        SubtaskIntegration::Conflict {
            files: vec!["a.txt".to_string()]
        }
    );
    // The task workspace is exactly as before the failed integration.
    assert_eq!(git(&f.task_ws.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(f.task_status(), "");
    assert_eq!(f.read("a.txt"), "line 1\nONE\nline 3\n");
    subs.discard(&two).await.unwrap();

    // The retry forks from the updated task workspace and integrates.
    let retry = subs.open(&f.task_ws, "s2-a2").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(retry.root.join("a.txt")).unwrap(),
        "line 1\nONE\nline 3\n"
    );
    std::fs::write(retry.root.join("a.txt"), "line 1\nONE\nline 3\nTWO\n").unwrap();
    let again = subs
        .integrate(&f.task_ws, &retry, "subtask 2")
        .await
        .unwrap();
    assert!(matches!(again, SubtaskIntegration::Integrated { .. }));
    assert_eq!(f.read("a.txt"), "line 1\nONE\nline 3\nTWO\n");
}

#[tokio::test]
async fn empty_attempt_integrates_nothing_and_leftovers_are_cleaned() {
    let f = Fixture::new().await;
    let subs = GitSubtaskWorkspaces::new();
    let head = git(&f.task_ws.root, &["rev-parse", "HEAD"]);
    let idle = subs.open(&f.task_ws, "s1-a1").await.unwrap();
    assert_eq!(
        subs.integrate(&f.task_ws, &idle, "nothing").await.unwrap(),
        SubtaskIntegration::Integrated { commit: None }
    );
    assert_eq!(git(&f.task_ws.root, &["rev-parse", "HEAD"]), head);

    // Reopening a label replaces what a crashed process left.
    let stale = subs.open(&f.task_ws, "s2-a1").await.unwrap();
    std::fs::write(stale.root.join("junk.txt"), "junk\n").unwrap();
    let fresh = subs.open(&f.task_ws, "s2-a1").await.unwrap();
    assert!(!fresh.root.join("junk.txt").exists());

    // An orphan directory git does not know about.
    let orphan = GitSubtaskWorkspaces::subtask_root(&f.task_ws.root, "s9-a9");
    std::fs::create_dir_all(orphan.join("x")).unwrap();

    subs.discard_all(&f.task_ws).await.unwrap();
    assert!(!idle.root.exists() && !fresh.root.exists() && !orphan.exists());
    assert_eq!(git(&f.task_ws.root, &["branch", "--list", "*--s*"]), "");
    // The task workspace itself is untouched.
    assert!(f.task_ws.root.join("a.txt").exists());
}

#[tokio::test]
async fn labels_and_branchless_tasks_are_rejected() {
    let f = Fixture::new().await;
    let subs = GitSubtaskWorkspaces::new();
    assert!(subs.open(&f.task_ws, "../escape").await.is_err());
    let mut no_branch = f.task_ws.clone();
    no_branch.branch = None;
    assert!(subs.open(&no_branch, "s1-a1").await.is_err());
}

const PREAMBLE: &str =
    "shared header line one\nshared header line two\nshared header line three\nshared four\n";

/// Answers every resolution request with both edits merged.
struct ResolvingProvider;

#[async_trait::async_trait]
impl vibe_core::ModelProvider for ResolvingProvider {
    fn info(&self) -> vibe_core::ProviderInfo {
        vibe_core::ProviderInfo {
            name: "resolver".into(),
            supports_tools: false,
            supports_thinking: false,
            default_model: "m".into(),
        }
    }

    async fn complete(
        &self,
        _request: vibe_core::CompletionRequest,
    ) -> vibe_core::Result<vibe_core::CompletionResponse> {
        Ok(vibe_core::CompletionResponse {
            message: vibe_core::Message::assistant(format!(
                "{PREAMBLE}line 1\nONE and TWO\nline 3\n"
            )),
            stop_reason: vibe_core::StopReason::EndTurn,
            usage: vibe_core::Usage::default(),
            model: "m".into(),
        })
    }
}

#[tokio::test]
async fn assisted_strategy_resolves_integration_conflicts() {
    let f = Fixture::new().await;
    std::fs::write(
        f.task_ws.root.join("a.txt"),
        format!("{PREAMBLE}line 1\nline 2\nline 3\n"),
    )
    .unwrap();
    git(&f.task_ws.root, &["add", "-A"]);
    git(&f.task_ws.root, &["commit", "-qm", "preamble"]);
    let subs =
        GitSubtaskWorkspaces::new().with_merge_strategy(vibe_workspace::MergeStrategy::Assisted {
            provider: std::sync::Arc::new(ResolvingProvider),
            model: "m".into(),
        });
    let one = subs.open(&f.task_ws, "s1-a1").await.unwrap();
    let two = subs.open(&f.task_ws, "s2-a1").await.unwrap();
    std::fs::write(
        one.root.join("a.txt"),
        format!("{PREAMBLE}line 1\nONE\nline 3\n"),
    )
    .unwrap();
    std::fs::write(
        two.root.join("a.txt"),
        format!("{PREAMBLE}line 1\nTWO\nline 3\n"),
    )
    .unwrap();
    subs.integrate(&f.task_ws, &one, "subtask 1").await.unwrap();
    let second = subs.integrate(&f.task_ws, &two, "subtask 2").await.unwrap();
    assert!(
        matches!(second, SubtaskIntegration::Integrated { commit: Some(_) }),
        "{second:?}"
    );
    assert_eq!(
        f.read("a.txt"),
        format!("{PREAMBLE}line 1\nONE and TWO\nline 3\n")
    );
    assert_eq!(f.task_status(), "");
    let parents = git(&f.task_ws.root, &["rev-list", "--parents", "-1", "HEAD"]);
    assert_eq!(parents.split(' ').count(), 3, "a merge commit");
}
