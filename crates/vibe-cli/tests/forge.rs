//! `vibe task import` and `vibe pr` against a simulated GitHub API.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn git(root: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
}

impl Project {
    fn new(api: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let origin = dir.path().join("origin.git");
        std::fs::create_dir(&root).unwrap();
        git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README.md"), "# demo\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);
        git(
            &root,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&root, &["push", "-q", "origin", "main"]);
        let p = Self {
            _dir: dir,
            root,
            origin,
        };
        p.vibe().arg("init").assert().success();
        p.vibe()
            .args(["config", "set", "integrations.github.api_url", api])
            .assert()
            .success();
        p
    }

    fn vibe(&self) -> Command {
        let mut cmd = Command::cargo_bin("vibe").unwrap();
        cmd.current_dir(&self.root)
            .env("HOME", self.root.join(".home"))
            .env("NO_COLOR", "1")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY");
        cmd
    }

    fn json(&self, cmd: &mut Command) -> Value {
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

async fn issue_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/app/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 7, "title": "Fix typo in README", "body": "teh -> the",
            "labels": [{"name": "docs"}, {"name": "good first issue"}],
            "html_url": "https://github.com/octo/app/issues/7"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/app/issues/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 8, "title": "A pull request", "body": "",
            "pull_request": {}, "html_url": "https://github.com/octo/app/pull/8"
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn import_creates_one_task_per_issue() {
    let server = issue_server().await;
    let p = Project::new(&server.uri());
    let created = p.json(p.vibe().args(["--json", "task", "import", "octo/app#7"]));
    assert_eq!(created["number"], 1);
    assert_eq!(created["task"]["title"], "Fix typo in README");
    assert_eq!(
        created["task"]["labels"],
        json!(["docs", "good first issue"])
    );
    assert_eq!(
        created["task"]["source"],
        json!({"kind": "issue", "provider": "github", "reference": "octo/app#7"})
    );
    assert!(
        created["task"]["description"]
            .as_str()
            .unwrap()
            .contains("teh -> the")
    );
    p.vibe()
        .args(["task", "import", "https://github.com/octo/app/issues/7"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already imported as"));
    p.vibe()
        .args(["task", "import", "octo/app#8"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is a pull request"));
}

#[tokio::test]
async fn pr_pushes_the_branch_and_links_the_issue() {
    let server = issue_server().await;
    Mock::given(method("POST"))
        .and(path("/repos/octo/app/pulls"))
        .and(header("authorization", "Bearer ghp_test"))
        .and(body_partial_json(
            json!({"base": "main", "draft": false, "title": "Fix typo in README"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "html_url": "https://github.com/octo/app/pull/42"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let p = Project::new(&server.uri());
    p.vibe()
        .args(["task", "import", "octo/app#7"])
        .assert()
        .success();
    // Not ready yet.
    p.vibe()
        .args(["pr", "1", "--repo", "octo/app"])
        .env("GITHUB_TOKEN", "ghp_test")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not ready"));
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--complexity", "trivial"])
        .assert()
        .success();
    // A token is required to open it.
    p.vibe()
        .args(["pr", "1", "--repo", "octo/app", "--no-push"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("needs a token"));
    let opened = p.json(
        p.vibe()
            .args(["--json", "pr", "1", "--repo", "octo/app"])
            .env("GITHUB_TOKEN", "ghp_test"),
    );
    assert_eq!(opened["url"], "https://github.com/octo/app/pull/42");
    let branch = opened["branch"].as_str().unwrap();
    assert!(branch.starts_with("vibe/fix-typo-in-readme-"));
    assert!(
        git(&p.origin, &["branch", "--list"]).contains(branch.trim_start_matches("refs/heads/"))
    );

    let requests = server.received_requests().await.unwrap();
    let pull = requests
        .iter()
        .find(|r| r.url.path() == "/repos/octo/app/pulls")
        .unwrap();
    let body: Value = serde_json::from_slice(&pull.body).unwrap();
    assert_eq!(body["head"], branch);
    let text = body["body"].as_str().unwrap();
    assert!(text.contains("## Changes"), "{text}");
    assert!(text.contains("QA round 1: approved"), "{text}");
    assert!(text.contains("Closes #7"), "{text}");
}
