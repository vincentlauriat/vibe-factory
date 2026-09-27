//! End-to-end tests of `vibe serve` against the real binary.

#![allow(missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Server {
    child: Child,
    base: String,
    token: String,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(root)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn start() -> Server {
    start_with(&[])
}

fn start_with(extra: &[&str]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "# demo\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    let vibe = assert_cmd::cargo::cargo_bin("vibe");
    assert!(
        Command::new(&vibe)
            .arg("init")
            .current_dir(root)
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let mut child = Command::new(&vibe)
        .args([
            "--json",
            "serve",
            "--port",
            "0",
            "--provider",
            "mock",
            "--workspace",
            "in_place",
        ])
        .args(extra)
        .current_dir(root)
        .env("HOME", root.join(".home"))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let info: Value = serde_json::from_str(&first).expect("serve prints its address as JSON");
    Server {
        child,
        base: format!("http://127.0.0.1:{}", info["port"]),
        token: info["token"].as_str().unwrap().to_string(),
        _dir: dir,
    }
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn api_requires_the_token_and_the_local_host_name() {
    let s = start();
    let anonymous = reqwest::get(s.url("/api/health")).await.unwrap();
    assert_eq!(anonymous.status(), 401);
    let wrong = reqwest::Client::new()
        .get(s.url("/api/health"))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let ok = s.get("/api/health").await;
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.json::<Value>().await.unwrap()["name"], "vibe");
    // Tokens in the query string are only for event streams.
    let query = reqwest::get(s.url(&format!("/api/tasks?token={}", s.token)))
        .await
        .unwrap();
    assert_eq!(query.status(), 401);
    let rebinding = reqwest::Client::new()
        .get(s.url("/api/health"))
        .bearer_auth(&s.token)
        .header("Host", "attacker.example:80")
        .send()
        .await
        .unwrap();
    assert_eq!(rebinding.status(), 403);
    let page = reqwest::get(s.url("/")).await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("<title>"));
}

#[tokio::test]
async fn create_run_and_follow_a_task() {
    let s = start();
    let created = s
        .post(
            "/api/tasks",
            json!({"title": "Fix typo in README", "description": "teh -> the"}),
        )
        .await;
    assert_eq!(created.status(), 201);
    assert_eq!(created.json::<Value>().await.unwrap()["number"], 1);
    assert_eq!(
        s.post("/api/tasks", json!({"title": " "})).await.status(),
        400
    );
    assert_eq!(s.get("/api/tasks/99").await.status(), 404);

    // Subscribe before running: the stream then follows the new run.
    let mut stream = reqwest::Client::new()
        .get(s.url(&format!("/api/tasks/1/stream?token={}", s.token)))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    assert_eq!(s.post("/api/tasks/1/run", json!({})).await.status(), 202);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while !seen.contains("event: run_finished") {
        assert!(Instant::now() < deadline, "no run_finished in:\n{seen}");
        let chunk = tokio::time::timeout(Duration::from_secs(10), stream.chunk())
            .await
            .expect("stream stalled")
            .unwrap()
            .expect("stream ended");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(seen.contains("event: run_started"));
    assert!(seen.contains("id: 1\n"));

    let task: Value = s.get("/api/tasks/1").await.json().await.unwrap();
    assert_eq!(task["task"]["status"], "ready");
    assert_eq!(task["run"]["status"], "finished");
    assert!(task["plan"].is_object());
    let events: Vec<Value> = s.get("/api/tasks/1/events").await.json().await.unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    let later: Vec<Value> = s
        .get("/api/tasks/1/events?after=3")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(later[0]["seq"], 4);
    let listed: Vec<Value> = s.get("/api/tasks").await.json().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["running"], false);
    // Nothing waits for an approval.
    assert_eq!(
        s.post("/api/tasks/1/approve", json!({})).await.status(),
        409
    );
}

#[tokio::test]
async fn evaluation_summaries_are_listed() {
    let results = tempfile::tempdir().unwrap();
    let suite = results.path().join("2026-10-01-model");
    std::fs::create_dir_all(&suite).unwrap();
    std::fs::write(
        suite.join("summary.json"),
        json!({"schema_version": 1, "reports": 2, "framework_commits": ["abc"],
               "rows": [{"provider": "mock", "model": null, "case": "ALL", "runs": 2, "successes": 1,
                         "success_rate": 0.5, "mean_seconds": 1.0, "median_seconds": 1.0,
                         "total_tokens": 10, "mean_tokens": 5, "runs_without_usage": 0,
                         "mean_validation_attempts": 1.0}]})
        .to_string(),
    )
    .unwrap();
    let s = start_with(&["--evals", results.path().to_str().unwrap()]);
    let data: Value = s.get("/api/evals").await.json().await.unwrap();
    assert_eq!(data["enabled"], true);
    assert_eq!(data["suites"][0]["name"], "2026-10-01-model");
    assert_eq!(data["suites"][0]["summary"]["rows"][0]["successes"], 1);
    let off: Value = start().get("/api/evals").await.json().await.unwrap();
    assert_eq!(off["enabled"], false);
}

#[test]
fn closing_stdin_stops_the_server_with_exit_on_stdin_eof() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "# demo\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    let vibe = assert_cmd::cargo::cargo_bin("vibe");
    assert!(
        Command::new(&vibe)
            .arg("init")
            .current_dir(root)
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let mut child = Command::new(&vibe)
        .args([
            "--json",
            "serve",
            "--exit-on-stdin-eof",
            "--port",
            "0",
            "--provider",
            "mock",
        ])
        .current_dir(root)
        .env("HOME", root.join(".home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let info: Value = serde_json::from_str(&first).expect("serve prints its address as JSON");
    assert!(info["port"].as_u64().unwrap() > 0);
    let token_file = root.join(".vibe").join("server.token");
    assert!(token_file.exists());

    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the server did not stop within 5 s of stdin closing");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
    assert!(!token_file.exists(), "the token file is removed on exit");
}
