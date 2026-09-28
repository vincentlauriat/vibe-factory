//! End-to-end tests of `vibe serve` against the real binary.

#![allow(missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vibe_pipeline::{EventCursor, RunTrace, TaggedEnvelope};

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
    let mut args = vec!["--provider", "mock", "--workspace", "in_place"];
    args.extend_from_slice(extra);
    serve_in(new_project(), &args)
}

/// A git repository with one commit, initialised for vibe.
fn new_project() -> tempfile::TempDir {
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
    dir
}

fn serve_in(dir: tempfile::TempDir, args: &[&str]) -> Server {
    let root = dir.path();
    let vibe = assert_cmd::cargo::cargo_bin("vibe");
    let mut child = Command::new(&vibe)
        .args(["--json", "serve", "--port", "0"])
        .args(args)
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
    // Not a number, a directory or a hex id prefix: `99` would match an id
    // starting with `99` once in 256 runs.
    assert_eq!(s.get("/api/tasks/zzz").await.status(), 404);

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

impl Server {
    fn root(&self) -> &std::path::Path {
        self._dir.path()
    }

    /// Start a run of `task` and wait for it to finish.
    async fn run_to_the_end(&self, task: u32) {
        let started = self
            .post(&format!("/api/tasks/{task}/run"), json!({}))
            .await;
        assert_eq!(started.status(), 202);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let shown: Value = self
                .get(&format!("/api/tasks/{task}"))
                .await
                .json()
                .await
                .unwrap();
            if shown["running"] == false && shown["run"]["status"] == "finished" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "run of #{task} not finished: {shown}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn events(&self, query: &str) -> Vec<TaggedEnvelope> {
        let response = self.get(&format!("/api/events{query}")).await;
        assert_eq!(response.status(), 200, "/api/events{query}");
        response.json().await.unwrap()
    }
}

/// A server whose mock provider plays `script` (one run).
fn start_scripted(script: &Value) -> Server {
    let dir = new_project();
    let path = dir.path().join(".vibe").join("test-script.json");
    std::fs::write(&path, script.to_string()).unwrap();
    let path = path.to_str().unwrap().to_string();
    serve_in(dir, &["--workspace", "in_place", "--script", &path])
}

fn fenced(value: Value) -> Value {
    json!({"text": format!("Done.\n\n```json\n{value}\n```")})
}

/// Plan one subtask that writes `hello.txt` with `write_file`, then approve.
fn greeting_script() -> Value {
    json!([
        fenced(json!({"complexity": "trivial", "confidence": 0.9, "reasoning": "one file",
                      "needs_research": false, "needs_critique": false, "risk_level": "low"})),
        fenced(json!({"approach": "write one file", "phases": [{"name": "Only", "subtasks": [
            {"title": "Write hello.txt", "description": "create the file",
             "files": ["hello.txt"], "verification": ["cat hello.txt"]}
        ]}]})),
        {"tool": "write_file", "input": {"path": "hello.txt", "content": "Hello, world!\n"}},
        fenced(json!({"status": "done", "summary": "wrote hello.txt",
                      "files_changed": ["hello.txt"], "notes": ""})),
        fenced(json!({"verdict": "approved", "summary": "looks good", "issues": []})),
    ])
}

/// The directory of task `number` under `.vibe/tasks`.
fn task_dir(root: &std::path::Path, number: u32) -> std::path::PathBuf {
    let prefix = format!("{number:03}-");
    std::fs::read_dir(root.join(".vibe").join("tasks"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .unwrap_or_else(|| panic!("no directory for task {number}"))
}

/// One server-sent event: name, id, data.
#[derive(Debug)]
struct Frame {
    event: String,
    id: Option<String>,
    data: String,
}

/// Read frames from an SSE response until `done` accepts the frames so far.
async fn read_frames(
    response: &mut reqwest::Response,
    done: impl Fn(&[Frame]) -> bool,
) -> Vec<Frame> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut buffer = String::new();
    let mut frames = Vec::new();
    while !done(&frames) {
        assert!(Instant::now() < deadline, "stream incomplete: {frames:#?}");
        let chunk = tokio::time::timeout(Duration::from_secs(20), response.chunk())
            .await
            .expect("stream stalled")
            .unwrap()
            .expect("stream ended");
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let mut frame = Frame {
                event: String::new(),
                id: None,
                data: String::new(),
            };
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    frame.event = v.to_string();
                } else if let Some(v) = line.strip_prefix("id: ") {
                    frame.id = Some(v.to_string());
                } else if let Some(v) = line.strip_prefix("data: ") {
                    frame.data.push_str(v);
                }
            }
            if !frame.data.is_empty() {
                frames.push(frame);
            }
        }
    }
    frames
}

#[tokio::test]
async fn the_page_has_every_view_and_listens_to_every_event_type() {
    let s = start();
    let page = reqwest::get(s.url("/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for needle in [
        "id=\"view-tasks\"",
        "id=\"view-activity\"",
        "id=\"view-history\"",
        "id=\"view-evals\"",
        "[\"trace\", \"Trace\"]",
        "/api/stream?",
    ] {
        assert!(page.contains(needle), "the page lacks {needle}");
    }
    // Named SSE events only reach listeners registered for their name: the
    // listeners and the Activity filters come from `GROUPS`.
    let groups = page
        .split_once("const GROUPS = {")
        .and_then(|(_, rest)| rest.split_once("};"))
        .expect("the page defines GROUPS")
        .0;
    for ty in vibe_core::Event::TYPES {
        assert!(groups.contains(&format!("\"{ty}\"")), "GROUPS lacks {ty}");
    }
}

#[tokio::test]
async fn global_events_are_filtered_and_resumable() {
    let s = start();
    s.post("/api/tasks", json!({"title": "Fix typo in README"}))
        .await;
    s.post("/api/tasks", json!({"title": "Never run"})).await;
    s.run_to_the_end(1).await;

    let response = s.get("/api/events").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-vibe-read-errors"], "0");
    let cursor = response.headers()["x-vibe-cursor"]
        .to_str()
        .unwrap()
        .to_string();
    let all: Vec<TaggedEnvelope> = response.json().await.unwrap();
    assert!(all.len() > 5);
    assert!(all.iter().all(|e| e.number == 1));
    assert!(all.windows(2).all(|w| w[0].cursor() < w[1].cursor()));
    assert_eq!(all.last().unwrap().cursor().to_string(), cursor);
    assert_eq!(
        cursor.parse::<EventCursor>().unwrap(),
        all.last().unwrap().cursor()
    );

    let finished = s.events("?type=run_finished").await;
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].envelope.event.type_name(), "run_finished");
    let two = s.events("?task=1&type=run_started&type=run_finished").await;
    assert_eq!(two.len(), 2);
    assert!(s.events("?task=2").await.is_empty());
    let last3 = s.events("?limit=3").await;
    assert_eq!(last3, all[all.len() - 3..]);
    let after = s.events(&format!("?after={}", all[2].cursor())).await;
    assert_eq!(after, all[3..]);
    assert!(s.events("?since=2999-01-01T00:00:00Z").await.is_empty());
    assert_eq!(s.events("?since=2000-01-01T00:00:00Z").await, all);
    // `since` takes a time or an age, as `vibe events --since`.
    assert_eq!(s.events("?since=1h").await, all);
    for bad in [
        "?type=run_done",
        "?after=yesterday",
        "?since=yesterday",
        "?limit=many",
    ] {
        assert_eq!(
            s.get(&format!("/api/events{bad}")).await.status(),
            400,
            "{bad}"
        );
    }
    assert_eq!(s.get("/api/events?task=zzz").await.status(), 404);
    // The per-task route keeps its shape: envelopes without a task.
    let own: Vec<Value> = s.get("/api/tasks/1/events").await.json().await.unwrap();
    assert!(own[0].get("task").is_none() && own[0].get("seq").is_some());

    // An unreadable log is counted, the others are still served.
    std::fs::create_dir(task_dir(s.root(), 2).join("events.jsonl")).unwrap();
    let response = s.get("/api/events").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-vibe-read-errors"], "1");
    let served: Vec<TaggedEnvelope> = response.json().await.unwrap();
    assert_eq!(served, all);
}

#[tokio::test]
async fn the_global_stream_replays_then_follows_new_tasks() {
    let s = start();
    s.post("/api/tasks", json!({"title": "Fix typo in README"}))
        .await;
    s.run_to_the_end(1).await;
    let all = s.events("").await;
    let from = all[2].cursor();

    let mut stream = reqwest::Client::new()
        .get(s.url(&format!("/api/stream?after={from}&token={}", s.token)))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let replayed = read_frames(&mut stream, |f| {
        f.iter().filter(|f| f.id.is_some()).count() >= all.len() - 3
    })
    .await;
    let logged: Vec<&Frame> = replayed.iter().filter(|f| f.id.is_some()).collect();
    assert_eq!(
        logged[0].id.as_deref(),
        Some(all[3].cursor().to_string().as_str())
    );

    // A task created and run while connected is followed from its start.
    s.post("/api/tasks", json!({"title": "Second task"})).await;
    assert_eq!(s.post("/api/tasks/2/run", json!({})).await.status(), 202);
    let frames = read_frames(&mut stream, |f| {
        f.iter()
            .any(|f| f.event == "run_finished" && f.data.contains("\"number\":2"))
    })
    .await;
    let mut last = from;
    let mut numbers = std::collections::BTreeSet::new();
    for frame in replayed.iter().chain(&frames) {
        let tagged: TaggedEnvelope = serde_json::from_str(&frame.data).unwrap();
        assert_eq!(frame.event, tagged.envelope.event.type_name());
        numbers.insert(tagged.number);
        match &frame.id {
            Some(id) => {
                let cursor: EventCursor = id.parse().expect("ids are event cursors");
                assert_eq!(cursor, tagged.cursor());
                assert!(cursor > last, "{cursor} after {last}");
                last = cursor;
            }
            None => assert_eq!(frame.event, "agent_delta", "only streamed text has no id"),
        }
    }
    assert_eq!(numbers, [1, 2].into());
    // The mock streams its text: tagged with the task, without an id.
    let delta = frames
        .iter()
        .find(|f| f.event == "agent_delta")
        .expect("streamed text of the run started while connected");
    assert!(delta.id.is_none());
    assert!(delta.data.contains("\"number\":2"), "{delta:?}");
    assert!(
        frames
            .iter()
            .any(|f| f.event == "run_started" && f.data.contains("\"number\":2"))
    );

    // Filters, and Last-Event-ID winning over a stale `after`.
    let every = s.events("").await;
    let mut filtered = reqwest::Client::new()
        .get(s.url(&format!(
            "/api/stream?after=0-0-0&type=run_finished&task=2&token={}",
            s.token
        )))
        .send()
        .await
        .unwrap();
    let first = read_frames(&mut filtered, |f| !f.is_empty()).await;
    assert_eq!(first[0].event, "run_finished");
    assert!(first[0].data.contains("\"number\":2"), "{:?}", first[0]);
    let before_last = every[every.len() - 2].cursor().to_string();
    let mut resumed = reqwest::Client::new()
        .get(s.url(&format!("/api/stream?after=0-0-0&token={}", s.token)))
        .header("Last-Event-ID", &before_last)
        .send()
        .await
        .unwrap();
    let first = read_frames(&mut resumed, |f| !f.is_empty()).await;
    assert_eq!(
        first[0].id.as_deref(),
        Some(every.last().unwrap().cursor().to_string().as_str())
    );
    let bad = reqwest::Client::new()
        .get(s.url(&format!("/api/stream?after=nope&token={}", s.token)))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let anonymous = reqwest::get(s.url("/api/stream")).await.unwrap();
    assert_eq!(anonymous.status(), 401);
}

#[tokio::test]
async fn history_and_trace_of_a_scripted_run() {
    let s = start_scripted(&greeting_script());
    s.post(
        "/api/tasks",
        json!({"title": "Write the greeting module for the demo application",
               "description": "Create hello.txt containing a friendly greeting so the demo can print it."}),
    )
    .await;
    s.post("/api/tasks", json!({"title": "Never run"})).await;
    s.run_to_the_end(1).await;

    // History: only finished tasks in the list, any task in the detail.
    let list: Vec<Value> = s.get("/api/history").await.json().await.unwrap();
    assert_eq!(list.len(), 1, "{list:#?}");
    let h = &list[0];
    assert_eq!(h["number"], 1);
    assert_eq!(h["runs"][0]["state"], "finished");
    assert!(h["totals"]["commits"].as_u64().unwrap() >= 1, "{h}");
    let files: Vec<&str> = h["changed_files"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"hello.txt"), "{h}");
    let all: Vec<Value> = s.get("/api/history?all=true").await.json().await.unwrap();
    assert_eq!(all.len(), 1);
    let one: vibe_pipeline::TaskHistory = s.get("/api/history/1").await.json().await.unwrap();
    assert_eq!(one.number, 1);
    assert!(!one.runs[0].commits.is_empty());
    let never: Value = s.get("/api/history/2").await.json().await.unwrap();
    assert_eq!(never["runs"], json!([]));
    assert_eq!(s.get("/api/history/zzz").await.status(), 404);

    // Trace of the last run, of a run by prefix, of every run.
    let trace: RunTrace = s.get("/api/tasks/1/trace").await.json().await.unwrap();
    assert_eq!(trace.files_written, ["hello.txt"]);
    let write = trace
        .calls
        .iter()
        .find(|c| c.tool == "write_file")
        .expect("a write_file call");
    assert_eq!(write.input["path"], "hello.txt");
    let prefix = &trace.run.to_string()[..8];
    let by_prefix: RunTrace = s
        .get(&format!("/api/tasks/1/trace?run={prefix}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(by_prefix, trace);
    let every: Vec<RunTrace> = s
        .get("/api/tasks/1/trace?all=true")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(every, std::slice::from_ref(&trace));
    assert_eq!(s.get("/api/tasks/1/trace?run=zzzz").await.status(), 404);
    assert_eq!(s.get("/api/tasks/2/trace").await.status(), 404);
    assert_eq!(s.get("/api/tasks/zzz/trace").await.status(), 404);

    // Complete output of a call, as text.
    let output = s
        .get(&format!("/api/tasks/1/trace/{}/output", write.call))
        .await;
    assert_eq!(output.status(), 200);
    assert!(
        output.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let text = output.text().await.unwrap();
    assert!(text.contains("hello.txt"), "{text}");
    assert_eq!(
        s.get("/api/tasks/1/trace/0123456789ab/output")
            .await
            .status(),
        404
    );
    for bad in [
        "xyz",
        "000000000000",
        "..%2F..%2FREADME.md",
        "0123456789abc",
    ] {
        assert_eq!(
            s.get(&format!("/api/tasks/1/trace/{bad}/output"))
                .await
                .status(),
            400,
            "{bad}"
        );
    }

    // A recorded path outside the trace store is never read.
    let secret_dir = tempfile::tempdir().unwrap();
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, "TOP SECRET").unwrap();
    let log = task_dir(s.root(), 1).join("events.jsonl");
    let text = std::fs::read_to_string(&log).unwrap();
    let returned: Value = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["event"]["type"] == "tool_returned")
        .unwrap();
    let mut forged = returned.clone();
    forged["seq"] = json!(10_000);
    forged["event"]["call"] = json!("abcdefabcdef");
    forged["event"]["output_file"] = json!(format!(".vibe/tool-output/../../{}", secret.display()));
    std::fs::write(&log, format!("{text}{forged}\n")).unwrap();
    // The forged call is in the trace, its path dropped by the read layer.
    let forged: RunTrace = s.get("/api/tasks/1/trace").await.json().await.unwrap();
    let call = forged
        .calls
        .iter()
        .find(|c| c.call.to_string() == "abcdefabcdef")
        .expect("the forged result is paired");
    assert_eq!(call.output_file, None);
    let refused = s.get("/api/tasks/1/trace/abcdefabcdef/output").await;
    assert_eq!(refused.status(), 404);
    let body = refused.text().await.unwrap();
    assert!(body.contains("not traced"), "{body}");
    assert!(!body.contains("TOP SECRET"));

    // Outputs past 8 MiB are cut and marked.
    let file = write.output_file.clone().expect("the output is traced");
    std::fs::write(&file, "x".repeat(9 * 1024 * 1024)).unwrap();
    let long = s
        .get(&format!("/api/tasks/1/trace/{}/output", write.call))
        .await;
    assert_eq!(long.status(), 200);
    assert_eq!(long.headers()["x-vibe-truncated"], "true");
    let body = long.text().await.unwrap();
    assert!(body.len() < 8 * 1024 * 1024 + 100, "{}", body.len());
    assert!(body.ends_with("(output cut: 1048576 more bytes)\n"));

    // Nor is a trace file replaced by a link that leads out of it; the
    // answer is that of a missing output, without any path.
    #[cfg(unix)]
    {
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&secret, &file).unwrap();
        let refused = s
            .get(&format!("/api/tasks/1/trace/{}/output", write.call))
            .await;
        assert_eq!(refused.status(), 404);
        let body = refused.text().await.unwrap();
        assert!(!body.contains("TOP SECRET"));
        assert!(!body.contains("tool-output"), "{body}");
    }
}

#[tokio::test]
async fn new_routes_need_the_token_in_the_header() {
    let s = start();
    s.post("/api/tasks", json!({"title": "Fix typo in README"}))
        .await;
    s.run_to_the_end(1).await;
    let routes = [
        "/api/events",
        "/api/history",
        "/api/history/1",
        "/api/tasks/1/trace",
        "/api/tasks/1/trace/0123456789ab/output",
        "/api/stream",
    ];
    for route in routes {
        let anonymous = reqwest::get(s.url(route)).await.unwrap();
        assert_eq!(anonymous.status(), 401, "{route}");
    }
    // `?token=` is only for the two event streams.
    for route in &routes[..5] {
        let url = s.url(&format!("{route}?token={}", s.token));
        assert_eq!(reqwest::get(url).await.unwrap().status(), 401, "{route}");
    }
    let history_stream = reqwest::get(s.url(&format!("/api/history/stream?token={}", s.token)))
        .await
        .unwrap();
    assert_eq!(history_stream.status(), 401);
}

#[tokio::test]
async fn an_ambiguous_task_reference_is_a_bad_request() {
    let s = start();
    // Id prefixes a task number cannot shadow: letters, and `0`.
    let mut seen: std::collections::HashMap<char, u32> = std::collections::HashMap::new();
    let prefix = loop {
        let row: Value = s
            .post("/api/tasks", json!({"title": "Some task"}))
            .await
            .json()
            .await
            .unwrap();
        let first = row["task"]["id"].as_str().unwrap().chars().next().unwrap();
        if matches!(first, 'a'..='f' | '0') {
            let count = seen.entry(first).or_default();
            *count += 1;
            if *count == 2 {
                break first;
            }
        }
        assert!(row["number"].as_u64().unwrap() < 200);
    };
    for route in [
        format!("/api/events?task={prefix}"),
        format!("/api/history/{prefix}"),
        format!("/api/tasks/{prefix}"),
    ] {
        assert_eq!(s.get(&route).await.status(), 400, "{route}");
    }
}

#[tokio::test]
async fn many_clients_share_the_stream_and_a_replaced_log_is_not_sent_twice() {
    let s = start();
    s.post("/api/tasks", json!({"title": "Fix typo in README"}))
        .await;
    s.run_to_the_end(1).await;
    let logged = s.events("").await;
    let open = || {
        reqwest::Client::new()
            .get(s.url(&format!("/api/stream?after=0-0-0&token={}", s.token)))
            .send()
    };
    let mut clients = Vec::new();
    for _ in 0..3 {
        let mut client = open().await.unwrap();
        assert_eq!(client.status(), 200);
        let replayed = read_frames(&mut client, |f| f.len() >= logged.len()).await;
        assert_eq!(replayed.len(), logged.len());
        clients.push(client);
    }

    // The log of #1 replaced by a copy (another file): the shared reader
    // reads it again from its start, and sends none of it again.
    let log = task_dir(s.root(), 1).join("events.jsonl");
    let copy = log.with_extension("copy");
    std::fs::copy(&log, &copy).unwrap();
    std::fs::rename(&copy, &log).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    s.post("/api/tasks", json!({"title": "Second task"})).await;
    assert_eq!(s.post("/api/tasks/2/run", json!({})).await.status(), 202);
    for client in &mut clients {
        let frames = read_frames(client, |f| {
            f.iter()
                .any(|f| f.event == "run_finished" && f.data.contains("\"number\":2"))
        })
        .await;
        let again: Vec<&Frame> = frames
            .iter()
            .filter(|f| f.data.contains("\"number\":1"))
            .collect();
        assert!(again.is_empty(), "sent again: {again:#?}");
    }

    // Past the limit of open streams: 503.
    let mut more = Vec::new();
    loop {
        let response = open().await.unwrap();
        if response.status() == 503 {
            break;
        }
        assert_eq!(response.status(), 200);
        more.push(response);
        assert!(more.len() <= 32, "no limit on open streams");
    }
    assert_eq!(clients.len() + more.len(), 32);
}
