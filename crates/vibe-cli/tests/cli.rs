//! End-to-end tests of the `vibe` binary, run in temporary git repositories.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};

/// A temporary git repository with one commit.
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let p = Self { dir };
        p.git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.root().join("README.md"), "# demo\n").unwrap();
        p.git(&["add", "README.md"]);
        p.git(&["commit", "-q", "-m", "init"]);
        p
    }

    /// A project that is not a git repository.
    fn plain() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(self.root())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `vibe` in this project, isolated from the user's environment.
    fn vibe(&self) -> Command {
        let home = self.root().join(".home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::cargo_bin("vibe").unwrap();
        cmd.current_dir(self.root())
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("RUST_LOG")
            .env("NO_COLOR", "1")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");
        cmd
    }

    fn init(&self) {
        self.vibe().arg("init").assert().success();
    }

    fn add_task(&self, title: &str, description: &str) {
        self.vibe()
            .args(["task", "add", title, "-d", description])
            .assert()
            .success();
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.vibe().arg("--json").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "vibe {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn task_dir(&self, number: u32) -> PathBuf {
        let tasks = self.root().join(".vibe").join("tasks");
        std::fs::read_dir(&tasks)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{number:03}-"))
            })
            .unwrap_or_else(|| panic!("no directory for task {number}"))
    }

    fn task_json(&self, number: u32) -> Value {
        let text = std::fs::read_to_string(self.task_dir(number).join("task.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn write_script(&self, name: &str, script: &Value) -> PathBuf {
        let path = self.root().join(name);
        std::fs::write(&path, serde_json::to_string_pretty(script).unwrap()).unwrap();
        path
    }
}

/// A scripted answer holding `value` in a json fence, as structured agents
/// produce.
fn fenced(value: Value) -> Value {
    json!({"text": format!("Done.\n\n```json\n{value}\n```")})
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[test]
fn help_lists_every_command() {
    let p = Project::plain();
    let assert = p.vibe().arg("--help").assert().success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for cmd in [
        "init",
        "task",
        "run",
        "status",
        "config",
        "agents",
        "plugins",
        "doctor",
        "completions",
    ] {
        assert!(out.contains(cmd), "missing {cmd} in help:\n{out}");
    }
}

#[test]
fn init_writes_config_and_gitignore_and_refuses_to_overwrite() {
    let p = Project::new();
    p.vibe()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Next steps"));
    let config = p.root().join(".vibe/config.toml");
    assert!(exists(&config));
    let gitignore = std::fs::read_to_string(p.root().join(".vibe/.gitignore")).unwrap();
    assert!(gitignore.contains("worktrees/") && gitignore.contains("tool-output/"));
    assert!(gitignore.contains("# tasks/ is committed on purpose"));
    assert!(!gitignore.lines().any(|l| l.trim() == "tasks/"));

    p.vibe()
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    std::fs::write(&config, "default_provider = \"openai\"\n").unwrap();
    p.vibe().args(["init", "--force"]).assert().success();
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("default_provider = \"anthropic\""));
    // Idempotent .gitignore.
    let again = std::fs::read_to_string(p.root().join(".vibe/.gitignore")).unwrap();
    assert_eq!(again.matches("worktrees/").count(), 1);
    assert_eq!(again.matches("# tasks/").count(), 1);
}

#[test]
fn init_outside_git_skips_gitignore() {
    let p = Project::plain();
    p.vibe()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("not a git repository"));
    assert!(!exists(&p.root().join(".vibe/.gitignore")));
}

#[test]
fn task_add_list_show() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args([
            "task",
            "add",
            "Add a --json flag to the export command",
            "-d",
            "Print JSON when --json is passed.",
            "--label",
            "cli",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("created task #1"));
    p.vibe()
        .args(["task", "add", "Second task", "-d", "-"])
        .write_stdin("Description from stdin\n")
        .assert()
        .success();
    std::fs::write(p.root().join("desc.md"), "From a file").unwrap();
    p.vibe()
        .args(["task", "add", "Third task", "--description-file", "desc.md"])
        .assert()
        .success();

    p.vibe()
        .args(["task", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Add a --json flag"))
        .stdout(predicate::str::contains("Second task"))
        .stdout(predicate::str::contains("backlog"));

    let list = p.json(&["task", "list"]);
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["number"], 1);
    assert_eq!(items[1]["task"]["description"], "Description from stdin");
    assert_eq!(items[2]["task"]["description"], "From a file");
    assert_eq!(items[0]["task"]["labels"], json!(["cli"]));

    let filtered = p.json(&["task", "list", "--status", "ready"]);
    assert!(filtered.as_array().unwrap().is_empty());

    // By number, directory name and id prefix.
    let id = items[1]["task"]["id"].as_str().unwrap().to_string();
    p.vibe()
        .args(["task", "show", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Print JSON when --json is passed.",
        ))
        .stdout(predicate::str::contains("not assessed"));
    let dir_name = p
        .task_dir(1)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let shown = p.json(&["task", "show", &dir_name]);
    assert_eq!(shown["number"], 1);
    let shown = p.json(&["task", "show", &id[..8]]);
    assert_eq!(shown["task"]["title"], "Second task");

    p.vibe()
        .args(["task", "show", "42"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no task matches `42`"));
    p.vibe().args(["task", "add", "   "]).assert().failure();
}

#[test]
fn config_show_path_and_set() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
    p.vibe()
        .args(["config", "show", "--default"])
        .assert()
        .success()
        .stdout(predicate::str::contains("default_provider = \"anthropic\""));

    p.vibe()
        .args(["config", "set", "pipeline.auto_merge", "true"])
        .assert()
        .success();
    p.vibe()
        .args(["config", "set", "phases.plan.model", "openai/gpt-5"])
        .assert()
        .success();
    let cfg = p.json(&["config", "show"]);
    assert_eq!(cfg["pipeline"]["auto_merge"], true);
    assert_eq!(cfg["phases"]["plan"]["model"], "openai/gpt-5");
    let text = std::fs::read_to_string(p.root().join(".vibe/config.toml")).unwrap();
    assert!(
        text.starts_with("# Vibe Factory configuration."),
        "comments kept"
    );

    // An invalid value is rejected and the file is left untouched.
    p.vibe()
        .args(["config", "set", "pipeline.max_qa_rounds", "many"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
    assert_eq!(
        std::fs::read_to_string(p.root().join(".vibe/config.toml")).unwrap(),
        text
    );
}

#[test]
fn config_set_before_init_keeps_plugin_tables_possible() {
    let p = Project::new();
    p.vibe()
        .args(["config", "set", "pipeline.max_qa_rounds", "5"])
        .assert()
        .success();
    let path = p.root().join(".vibe/config.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("plugins = []"));
    text.push_str("\n[[plugins]]\nname = \"later\"\ncommand = [\"x\"]\n");
    std::fs::write(&path, text).unwrap();
    let cfg = p.json(&["config", "show"]);
    assert_eq!(cfg["pipeline"]["max_qa_rounds"], 5);
    assert_eq!(cfg["plugins"][0]["name"], "later");
}

#[test]
fn assisted_merge_strategy_is_wired() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args(["config", "set", "pipeline.merge_strategy", "assisted"])
        .assert()
        .success();
    p.add_task(
        "Add a --json flag to the export command",
        "The export command should print JSON when --json is passed.",
    );
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .success();
    // `--script` implies the mock provider and cannot be combined with it.
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--script", "x.json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn agents_list_show_export() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args(["agents", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("planner"))
        .stdout(predicate::str::contains("builtin"));
    p.vibe()
        .args(["agents", "show", "planner"])
        .assert()
        .success()
        .stdout(predicate::str::contains("You are the planner"));
    p.vibe()
        .args(["agents", "show", "nobody"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown agent"));

    p.vibe()
        .args(["agents", "export", "planner"])
        .assert()
        .success();
    let toml_path = p.root().join(".vibe/agents/planner.toml");
    assert!(exists(&toml_path));
    assert!(exists(&p.root().join(".vibe/agents/planner.md")));
    // Unchanged export is still the built-in agent.
    let agents = p.json(&["agents", "list"]);
    let planner = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["role"] == "planner")
        .unwrap()
        .clone();
    assert_eq!(planner["source"], "builtin");

    let text = std::fs::read_to_string(&toml_path).unwrap();
    std::fs::write(
        &toml_path,
        text.replace("thinking = \"high\"", "thinking = \"max\""),
    )
    .unwrap();
    let agents = p.json(&["agents", "list"]);
    let planner = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["role"] == "planner")
        .unwrap()
        .clone();
    assert_eq!(planner["source"], "override");
    assert_eq!(planner["thinking"], "max");

    p.vibe()
        .args(["agents", "export", "planner"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn doctor_reports_missing_keys_and_passes_with_one() {
    let p = Project::new();
    p.init();
    p.vibe()
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("ANTHROPIC_API_KEY"))
        .stdout(predicate::str::contains("✓ git"));
    p.vibe()
        .arg("doctor")
        .env("ANTHROPIC_API_KEY", "sk-test")
        .assert()
        .success()
        .stdout(predicate::str::contains("All checks passed"));
    let out = p
        .vibe()
        .args(["--json", "doctor"])
        .env("ANTHROPIC_API_KEY", "sk-test")
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["ok"], true);

    // Broken configuration.
    std::fs::write(p.root().join(".vibe/config.toml"), "pipeline = 3\n").unwrap();
    p.vibe().arg("doctor").assert().code(1);
}

#[test]
fn doctor_fails_on_a_required_plugin_that_cannot_start() {
    let p = Project::new();
    p.init();
    let mut text = std::fs::read_to_string(p.root().join(".vibe/config.toml")).unwrap();
    text.push_str(
        "\n[[plugins]]\nname = \"ghost\"\ncommand = [\"definitely-not-a-vibe-plugin-binary\"]\nrequired = true\n",
    );
    std::fs::write(p.root().join(".vibe/config.toml"), text).unwrap();
    p.vibe()
        .args(["plugins", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ghost"));
    p.vibe()
        .args(["plugins", "check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("✗ ghost"));
    p.vibe()
        .arg("doctor")
        .env("ANTHROPIC_API_KEY", "sk-test")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("ghost"));
}

#[test]
fn dry_run_with_mock_then_resume_then_discard() {
    let p = Project::new();
    p.init();
    p.add_task(
        "Add a --json flag to the export command",
        "The export command should print JSON when --json is passed.",
    );

    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("── plan"))
        .stdout(predicate::str::contains("● planner"))
        .stdout(predicate::str::contains("--resume"));
    let dir = p.task_dir(1);
    assert!(exists(&dir.join("plan.json")));
    assert!(exists(&dir.join("spec.json")));
    assert!(!exists(&dir.join("qa_report_1.json")));
    assert_eq!(p.task_json(1)["status"], "backlog");

    p.vibe()
        .args(["run", "1", "--provider", "mock", "--resume"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status     ready"))
        .stdout(predicate::str::contains("git merge vibe/"));
    assert_eq!(p.task_json(1)["status"], "ready");
    assert!(exists(&dir.join("qa_report_1.json")));
    let branches = p.git(&["branch", "--list", "vibe/*"]);
    assert!(branches.contains("vibe/add-a-json-flag"), "{branches}");

    let status = p.json(&["status"]);
    assert_eq!(status["by_status"]["ready"], 1);
    let worktrees = status["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 1);
    let worktree = PathBuf::from(worktrees[0]["path"].as_str().unwrap());
    assert!(exists(&worktree));

    // A finished run cannot be resumed.
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--resume"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("already finished"));

    // Without --yes and without a terminal, discard refuses.
    p.vibe()
        .args(["task", "discard", "1"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--yes"));
    assert!(exists(&dir));

    p.vibe()
        .args(["task", "discard", "1", "--yes"])
        .assert()
        .success();
    assert!(!exists(&dir));
    assert!(p.git(&["branch", "--list", "vibe/*"]).trim().is_empty());
    assert!(!exists(&worktree), "worktree removed");
    assert!(
        p.json(&["task", "list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scripted_run_goes_through_every_phase_in_place() {
    let p = Project::new();
    p.init();
    p.add_task(
        "Write the greeting module for the demo application",
        "Create hello.txt containing a friendly greeting so the demo can print it.",
    );
    // In-place workspace, one subtask: the calls are strictly sequential.
    let script = p.write_script(
        "script.json",
        &json!([
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
        ]),
    );

    let out = p
        .vibe()
        .args(["--json", "run", "1", "--workspace", "in_place", "--script"])
        .arg(&script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = lines
        .iter()
        .filter_map(|l| l["event"]["type"].as_str())
        .collect();
    assert_eq!(types.first(), Some(&"run_started"));
    assert!(types.contains(&"tool_called"));
    assert!(types.contains(&"subtask_updated"));
    assert_eq!(types.last(), Some(&"run_finished"));
    let phases: Vec<&str> = lines
        .iter()
        .filter(|l| l["event"]["type"] == "phase_started")
        .filter_map(|l| l["event"]["phase"].as_str())
        .collect();
    assert_eq!(phases, ["assess", "plan", "build", "qa", "merge"]);
    let summary = lines.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["final_status"], "ready");
    assert_eq!(summary["exit_code"], 0);

    assert_eq!(
        std::fs::read_to_string(p.root().join("hello.txt")).unwrap(),
        "Hello, world!\n"
    );
    let task = p.task_json(1);
    assert_eq!(task["status"], "ready");
    assert_eq!(task["complexity"], "trivial");
    let dir = p.task_dir(1);
    for f in ["plan.json", "qa_report_1.json", "events.jsonl", "run.json"] {
        assert!(exists(&dir.join(f)), "missing {f}");
    }
    // Each event is logged exactly once.
    let log = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert_eq!(log.matches("\"run_started\"").count(), 1);
    // The subtask was committed in the project repository.
    let history = p.git(&["log", "--oneline"]);
    assert!(history.contains("complete subtask 1"), "{history}");
}

#[test]
fn failed_and_paused_runs_have_distinct_exit_codes() {
    let p = Project::new();
    p.init();
    p.add_task(
        "Refactor the configuration loader of the demo",
        "Split parsing from validation and keep the public behaviour identical.",
    );
    // Nothing scripted and no fallback: the spec phase cannot get an answer.
    let failing = p.write_script("fail.json", &json!({"fallback": false}));
    p.vibe()
        .args(["run", "1", "--workspace", "in_place", "--script"])
        .arg(&failing)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("status     failed"));
    assert_eq!(p.task_json(1)["status"], "failed");

    // QA keeps requesting the same change: the run pauses in review.
    p.add_task(
        "Harden the input validation of the import command",
        "Reject empty files and report the offending line numbers.",
    );
    let issue = json!({"verdict": "changes_requested", "summary": "not yet",
                       "issues": [{"severity": "high", "title": "Empty files accepted",
                                   "detail": "still accepted"}]});
    let fixed = json!({"status": "done", "summary": "fixed", "fixed": ["Empty files accepted"]});
    let paused = p.write_script(
        "pause.json",
        &json!({"routes": {
            "qa_reviewer": [fenced(issue.clone()), fenced(issue.clone()), fenced(issue.clone()), fenced(issue)],
            "qa_fixer": [fenced(fixed.clone()), fenced(fixed.clone()), fenced(fixed.clone()), fenced(fixed)]
        }}),
    );
    p.vibe()
        .args([
            "run",
            "2",
            "--workspace",
            "in_place",
            "--complexity",
            "simple",
            "--script",
        ])
        .arg(&paused)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("⏸ paused"));
    assert_eq!(p.task_json(2)["status"], "review");
}

#[test]
fn git_worktree_requires_a_repository() {
    let p = Project::plain();
    p.init();
    p.add_task("Anything at all for this test", "details");
    p.vibe()
        .args(["run", "1", "--provider", "mock"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn completions_and_unknown_provider() {
    let p = Project::new();
    p.vibe()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("vibe"));
    p.init();
    p.add_task("Anything at all for this test", "details");
    p.vibe()
        .args([
            "run",
            "1",
            "--provider",
            "nowhere",
            "--workspace",
            "in_place",
        ])
        .assert()
        .failure();
}

/// An approving reviewer cannot bypass required checks, even with auto-merge enabled.
#[test]
fn required_validation_blocks_merge_and_runs_again_on_resume() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args(["config", "set", "pipeline.max_validation_fix_attempts", "0"])
        .assert()
        .success();
    p.add_task("Fix typo in README", "Replace demo with corrected.");
    let script = p.write_script(
        "validation-script.json",
        &json!({"routes": {
            "coder": [
                {"tool": "write_file", "input": {"path": "README.md", "content": "corrected\n"}},
                fenced(json!({"status": "done", "summary": "corrected"}))
            ]
        }}),
    );
    p.vibe()
        .args([
            "config",
            "set",
            "pipeline.validation_commands",
            "[\"exit 0\", \"exit 7\", \"exit 0\"]",
        ])
        .assert()
        .success();
    let original = p.git(&["rev-parse", "HEAD"]);
    p.vibe()
        .args(["run", "1", "--auto-merge", "--script"])
        .arg(&script)
        .assert()
        .code(2);
    assert_eq!(p.git(&["rev-parse", "HEAD"]), original);
    assert_eq!(p.task_json(1)["status"], "review");
    let state_path = p.task_dir(1).join("run.json");
    let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["status"], "paused");
    assert_eq!(state["current_phase"], "merge");
    assert_eq!(state["validations"].as_array().unwrap().len(), 2);
    assert_eq!(state["validations"][0]["passed"], true);
    assert_eq!(state["validations"][1]["metadata"]["exit_code"], 7);
    p.vibe()
        .args([
            "config",
            "set",
            "pipeline.validation_commands",
            "[\"exit 0\"]",
        ])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--auto-merge", "--provider", "mock", "--resume"])
        .assert()
        .success();
    assert_eq!(p.task_json(1)["status"], "done");
    let state: Value = serde_json::from_slice(&std::fs::read(state_path).unwrap()).unwrap();
    assert_eq!(state["validations"].as_array().unwrap().len(), 4);
    assert_eq!(state["validations"][3]["integration"], true);
    assert_eq!(state["validations"][2]["passed"], true);
    assert_eq!(
        std::fs::read_to_string(p.root().join("README.md")).unwrap(),
        "corrected\n"
    );
}

#[test]
fn required_validation_blocks_ready_on_denied_or_empty_commands() {
    for command in ["sudo echo forbidden", ""] {
        let p = Project::new();
        p.init();
        p.add_task("Fix typo in README", "teh -> the");
        let commands = serde_json::to_string(&vec![command]).unwrap();
        p.vibe()
            .args(["config", "set", "pipeline.validation_commands", &commands])
            .assert()
            .success();
        p.vibe()
            .args(["run", "1", "--provider", "mock"])
            .assert()
            .code(2);
        assert_eq!(p.task_json(1)["status"], "review");
        let state: Value =
            serde_json::from_slice(&std::fs::read(p.task_dir(1).join("run.json")).unwrap())
                .unwrap();
        assert_eq!(state["validations"][0]["passed"], false);
        assert_eq!(state["validation_fix_attempts"], 0);
    }
}

#[cfg(unix)]
#[test]
fn required_validation_timeout_blocks_ready() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo", "teh -> the");
    p.vibe()
        .args(["config", "set", "security.command_timeout_secs", "1"])
        .assert()
        .success();
    p.vibe()
        .args([
            "config",
            "set",
            "pipeline.validation_commands",
            "[\"sleep 3\"]",
        ])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--provider", "mock"])
        .assert()
        .code(2);
    let state: Value =
        serde_json::from_slice(&std::fs::read(p.task_dir(1).join("run.json")).unwrap()).unwrap();
    assert_eq!(state["validations"][0]["metadata"]["timed_out"], true);
    assert_eq!(p.task_json(1)["status"], "review");
}

#[test]
fn validation_fixer_changes_workspace_and_needs_new_qa_approval_before_merge() {
    for verdict in ["approved", "inconclusive"] {
        let p = Project::new();
        p.init();
        p.add_task("Fix typo in README", "Replace the heading with corrected.");
        p.vibe()
            .args([
                "config",
                "set",
                "pipeline.validation_commands",
                "[\"git grep -q corrected -- README.md\"]",
            ])
            .assert()
            .success();
        let script = p.write_script("fix-validation.json", &json!({"routes": {
            "coder": [
                {"tool": "write_file", "input": {"path": "README.md", "content": "broken\n"}},
                fenced(json!({"status": "done", "summary": "edited"}))
            ],
            "qa_fixer": [
                {"tool": "write_file", "input": {"path": "README.md", "content": "corrected\n"}},
                fenced(json!({"status": "done", "summary": "corrected the content", "fixed": ["validation"]}))
            ],
            "qa_reviewer": [
                fenced(json!({"verdict": "approved", "summary": "initial review", "issues": []})),
                fenced(json!({"verdict": verdict, "summary": "review after correction", "issues": []}))
            ]
        }}));
        let original = p.git(&["rev-parse", "HEAD"]);
        p.vibe()
            .args(["run", "1", "--auto-merge", "--script"])
            .arg(script)
            .assert()
            .code(if verdict == "approved" { 0 } else { 2 });
        let state: Value =
            serde_json::from_slice(&std::fs::read(p.task_dir(1).join("run.json")).unwrap())
                .unwrap();
        assert_eq!(state["validation_fix_attempts"], 1);
        assert_eq!(state["qa_round"], 2);
        assert_eq!(state["validations"][0]["passed"], false);
        if verdict == "approved" {
            assert_eq!(p.task_json(1)["status"], "done");
            assert_eq!(state["validations"][1]["passed"], true);
            assert_eq!(
                std::fs::read_to_string(p.root().join("README.md")).unwrap(),
                "corrected\n"
            );
        } else {
            assert_eq!(p.task_json(1)["status"], "review");
            assert_eq!(p.git(&["rev-parse", "HEAD"]), original);
            assert_eq!(state["validations"].as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn integration_validation_rejects_combined_tree_and_rebuilds_it_on_resume() {
    let p = Project::new();
    std::fs::write(p.root().join("policy.txt"), "allowed\n").unwrap();
    p.git(&["add", "policy.txt"]);
    p.git(&["commit", "-qm", "initial policy"]);
    p.init();
    p.add_task("Fix typo in README", "Replace demo with corrected.");
    p.vibe()
        .args([
            "config",
            "set",
            "pipeline.validation_commands",
            "[\"git grep -q allowed -- policy.txt\"]",
        ])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .success();
    // The task branch still has the old policy. Only the combined tree will fail.
    std::fs::write(p.root().join("policy.txt"), "blocked\n").unwrap();
    p.git(&["add", "policy.txt"]);
    p.git(&["commit", "-qm", "base policy changed"]);
    let before = p.git(&["rev-parse", "HEAD"]);
    let script = p.write_script(
        "integration-script.json",
        &json!({"routes": {
            "coder": [
                {"tool": "write_file", "input": {"path": "README.md", "content": "corrected\n"}},
                fenced(json!({"status": "done", "summary": "corrected"}))
            ]
        }}),
    );
    p.vibe()
        .args(["run", "1", "--resume", "--auto-merge", "--script"])
        .arg(&script)
        .assert()
        .code(2);
    assert_eq!(p.git(&["rev-parse", "HEAD"]), before);
    assert_eq!(p.task_json(1)["status"], "review");
    let state_path = p.task_dir(1).join("run.json");
    let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["validations"][0]["passed"], true);
    assert_eq!(state["validations"][0]["integration"], false);
    assert_eq!(state["validations"][1]["passed"], false);
    assert_eq!(state["validations"][1]["integration"], true);
    assert_ne!(
        state["validations"][0]["workspace_root"],
        state["validations"][1]["workspace_root"]
    );
    assert_eq!(state["validation_fix_attempts"], 0);
    // A new target commit requires building and checking a fresh candidate on resume.
    std::fs::write(p.root().join("policy.txt"), "allowed again\n").unwrap();
    p.git(&["add", "policy.txt"]);
    p.git(&["commit", "-qm", "repair policy"]);
    p.vibe()
        .args(["run", "1", "--resume", "--auto-merge", "--provider", "mock"])
        .assert()
        .success();
    assert_eq!(p.task_json(1)["status"], "done");
    assert_eq!(
        std::fs::read_to_string(p.root().join("README.md")).unwrap(),
        "corrected\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.root().join("policy.txt")).unwrap(),
        "allowed again\n"
    );
    let state: Value = serde_json::from_slice(&std::fs::read(state_path).unwrap()).unwrap();
    assert_eq!(state["validations"].as_array().unwrap().len(), 4);
    assert_eq!(state["validations"][3]["integration"], true);
    assert_eq!(state["validations"][3]["passed"], true);
}

#[test]
fn parallel_subtasks_run_in_their_own_worktrees_and_are_integrated() {
    let p = Project::new();
    p.init();
    p.add_task("Add two files", "Create one.txt and two.txt.");
    // Both coder sessions pull from one queue in whatever order they run;
    // any interleaving writes both files and ends both sessions.
    let script = p.write_script(
        "parallel-script.json",
        &json!({"routes": {
            "planner": [fenced(json!({"approach": "two files", "phases": [
                {"name": "Both", "parallel": true, "subtasks": [
                    {"title": "Write one.txt", "description": "create it"},
                    {"title": "Write two.txt", "description": "create it"}
                ]}
            ]}))],
            "coder": [
                {"tool": "write_file", "input": {"path": "one.txt", "content": "one\n"}},
                fenced(json!({"status": "done", "summary": "written"})),
                {"tool": "write_file", "input": {"path": "two.txt", "content": "two\n"}},
                fenced(json!({"status": "done", "summary": "written"}))
            ],
            "qa_reviewer": [fenced(json!({"verdict": "approved", "summary": "ok", "issues": []}))]
        }}),
    );
    let out = p
        .vibe()
        .args(["run", "1", "--complexity", "trivial", "--script"])
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(p.task_json(1)["status"], "ready");

    let worktrees = p.root().join(".vibe").join("worktrees");
    let dirs: Vec<String> = std::fs::read_dir(&worktrees)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(dirs.len(), 1, "attempt worktrees were removed: {dirs:?}");
    let task_root = worktrees.join(&dirs[0]);
    assert_eq!(
        std::fs::read_to_string(task_root.join("one.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(task_root.join("two.txt")).unwrap(),
        "two\n"
    );
    assert_eq!(p.git(&["branch", "--list", "*--s*"]).trim(), "");
    // The project checkout was not touched.
    assert!(!p.root().join("one.txt").exists());
}
