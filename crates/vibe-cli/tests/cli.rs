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

    /// `vibe` as a plain process, with the same environment as
    /// [`Project::vibe`], for tests that manage its pipes or signals.
    fn vibe_process(&self) -> StdCommand {
        let home = self.root().join(".home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = StdCommand::new(assert_cmd::cargo::cargo_bin("vibe"));
        cmd.current_dir(self.root())
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("RUST_LOG")
            .env("NO_COLOR", "1")
            .env("HOME", &home);
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
        "events",
        "history",
        "trace",
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
    let recorded = p.task_json(1)["branch"].as_str().unwrap().to_string();
    assert!(branches.contains(&recorded), "{recorded} in {branches}");

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

    // Traced tool outputs are filed under the task directory's name.
    let trace = p
        .root()
        .join(".vibe")
        .join("tool-output")
        .join(dir.file_name().unwrap());
    std::fs::create_dir_all(trace.join("run")).unwrap();
    std::fs::write(trace.join("run").join("c.txt"), "output").unwrap();

    p.vibe()
        .args(["task", "discard", "1", "--yes"])
        .assert()
        .success();
    assert!(!exists(&dir));
    assert!(!exists(&trace), "tool outputs removed");
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

/// Image of the live container test, when the runtime has it locally.
#[cfg(unix)]
fn local_container_image() -> Option<String> {
    let image =
        std::env::var("VIBE_TEST_CONTAINER_IMAGE").unwrap_or_else(|_| "alpine:3.20".to_string());
    let ok = |args: &[&str]| {
        StdCommand::new("docker")
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !ok(&["info"]) || !ok(&["image", "inspect", &image]) {
        eprintln!("skipping: docker or the image {image} is not available locally");
        return None;
    }
    Some(image)
}

#[cfg(unix)]
#[test]
fn container_workspace_runs_commands_in_a_container() {
    let Some(image) = local_container_image() else {
        return;
    };
    let p = Project::new();
    p.init();
    for (key, value) in [
        ("pipeline.workspace", "container".to_string()),
        ("workspace.container.image", image.clone()),
        (
            "pipeline.validation_commands",
            "[\"grep -q container-ok out.txt\"]".to_string(),
        ),
    ] {
        p.vibe()
            .args(["config", "set", key, &value])
            .assert()
            .success();
    }
    p.add_task(
        "Record the environment",
        "Write out.txt from inside the sandbox.",
    );
    let script = p.write_script(
        "container-script.json",
        &json!({"routes": {
            "planner": [fenced(json!({"approach": "one command", "phases": [
                {"name": "Only", "subtasks": [{"title": "Write out.txt", "description": "run it"}]}
            ]}))],
            "coder": [
                {"tool": "bash", "input": {"command":
                    "test -f /.dockerenv && echo container-ok > out.txt; touch /etc/escaped 2>/dev/null || echo read-only >> out.txt"}},
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
    let task_root = std::fs::read_dir(&worktrees)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir() && !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(task_root.join("out.txt")).unwrap(),
        "container-ok\nread-only\n"
    );
    let state: Value =
        serde_json::from_slice(&std::fs::read(p.task_dir(1).join("run.json")).unwrap()).unwrap();
    assert_eq!(state["validations"][0]["passed"], true);
    assert_eq!(state["validations"][0]["metadata"]["runner"], "container");
    // No container is left behind.
    let left = StdCommand::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            "label=io.vibe-factory.managed=true",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&left.stdout).trim().is_empty());
}

#[test]
fn container_workspace_requires_a_valid_configuration() {
    let p = Project::new();
    p.init();
    p.add_task("Anything", "x");
    p.vibe()
        .args(["config", "set", "pipeline.workspace", "container"])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("[workspace.container]"));
    p.vibe()
        .args(["config", "set", "workspace.container.image", "alpine"])
        .assert()
        .success();
    p.vibe()
        .args(["config", "set", "workspace.container.network", "host"])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("namespace"));
}

#[test]
fn streamed_deltas_reach_json_output_but_not_the_event_log() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo in README", "teh -> the");
    let out = p
        .vibe()
        .args(["--json", "run", "1", "--provider", "mock", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"agent_delta\""), "{stdout}");
    let log = std::fs::read_to_string(p.task_dir(1).join("events.jsonl")).unwrap();
    assert!(!log.contains("agent_delta"));
    assert!(log.contains("\"agent_text\""));
}

#[test]
fn events_replays_numbered_events_and_follows_to_the_end() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo in README", "teh -> the");
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--dry-run"])
        .assert()
        .success();
    let out = p.vibe().args(["--json", "events", "1"]).output().unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    assert!(lines.iter().all(|l| l["schema"] == 2));
    assert_eq!(lines.last().unwrap()["event"]["type"], "run_finished");

    let after = p
        .vibe()
        .args(["--json", "events", "1", "--after", "3"])
        .output()
        .unwrap();
    let first: Value = serde_json::from_str(
        String::from_utf8_lossy(&after.stdout)
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["seq"], 4);

    // Following a finished run stops at its end.
    p.vibe()
        .args(["events", "1", "--follow"])
        .timeout(std::time::Duration::from_secs(20))
        .assert()
        .success()
        .stdout(predicate::str::contains("run finished"));
}

#[test]
fn approval_gate_pauses_and_approve_lets_the_run_continue() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo in README", "teh -> the");
    p.vibe()
        .args(["config", "set", "pipeline.approvals", "[\"plan\"]"])
        .assert()
        .success();
    p.vibe()
        .args(["run", "1", "--provider", "mock", "--workspace", "in_place"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("approval needed: the plan"));
    let state: Value =
        serde_json::from_slice(&std::fs::read(p.task_dir(1).join("run.json")).unwrap()).unwrap();
    assert_eq!(state["pending_approval"], "plan");

    p.vibe().args(["reject", "1"]).assert().failure();
    p.vibe()
        .args(["approve", "1", "--comment", "fine"])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan of task 1 approved"));
    p.vibe().args(["approve", "1"]).assert().code(1);
    p.vibe()
        .args([
            "run",
            "1",
            "--resume",
            "--provider",
            "mock",
            "--workspace",
            "in_place",
        ])
        .assert()
        .success();
    assert_eq!(p.task_json(1)["status"], "ready");

    let out = p.vibe().args(["--json", "events", "1"]).output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    assert!(
        lines
            .iter()
            .any(|l| l["event"]["type"] == "approval_resolved" && l["event"]["comment"] == "fine")
    );
}

#[cfg(unix)]
#[test]
fn cancel_stops_a_run_in_another_process() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo in README", "teh -> the");
    p.vibe()
        .args(["cancel", "1"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not running"));
    let script = p.write_script(
        "slow-script.json",
        &json!({"routes": {
            "planner": [fenced(json!({"approach": "wait", "phases": [
                {"name": "Only", "subtasks": [{"title": "Wait", "description": "sleep"}]}
            ]}))],
            "coder": [
                {"tool": "bash", "input": {"command": "sleep 2"}},
                {"tool": "bash", "input": {"command": "sleep 2"}},
                fenced(json!({"status": "done", "summary": "slept"}))
            ]
        }}),
    );
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("vibe"))
        .current_dir(p.root())
        .env("NO_COLOR", "1")
        .env("HOME", p.root().join(".home"))
        .args([
            "run",
            "1",
            "--complexity",
            "trivial",
            "--workspace",
            "in_place",
            "--script",
        ])
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let out = p.vibe().args(["cancel", "1", "--wait"]).output().unwrap();
        if out.status.success() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the run never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(p.task_json(1)["status"], "cancelled");
}

#[test]
fn memory_list_query_and_clear() {
    let p = Project::new();
    p.init();
    p.vibe()
        .args(["memory", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("empty"));
    let lines = [
        json!({"kind": "gotcha", "content": "Integration tests need DATABASE_URL", "recorded_at": "2026-09-01T10:00:00Z"}),
        json!({"kind": "pattern", "content": "Errors use anyhow", "recorded_at": "2026-09-02T10:00:00Z"}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(p.root().join(".vibe").join("memory.jsonl"), text).unwrap();
    let all = p.json(&["memory", "list"]);
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(all[0]["content"], "Errors use anyhow", "newest first");
    let hits = p.json(&["memory", "list", "--query", "flaky integration tests"]);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    p.vibe().args(["memory", "clear"]).assert().failure();
    p.vibe()
        .args(["memory", "clear", "--yes"])
        .assert()
        .success();
    assert_eq!(p.json(&["memory", "list"]).as_array().unwrap().len(), 0);
}

/// A run in place that writes `hello.txt` through one `write_file` call.
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

fn stdout_of(p: &Project, args: &[&str]) -> String {
    let out = p.vibe().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "vibe {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json_lines(p: &Project, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    stdout_of(p, &all)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Two tasks: #1 ran to `ready` in place (one commit), #2 only planned.
fn project_with_two_runs() -> Project {
    let p = Project::new();
    p.init();
    p.add_task(
        "Write the greeting module for the demo application",
        "Create hello.txt containing a friendly greeting so the demo can print it.",
    );
    let script = p.write_script("script.json", &greeting_script());
    p.vibe()
        .args(["run", "1", "--workspace", "in_place", "--script"])
        .arg(&script)
        .assert()
        .success();
    p.add_task("Fix typo in README", "teh -> the");
    p.vibe()
        .args([
            "run",
            "2",
            "--provider",
            "mock",
            "--dry-run",
            "--workspace",
            "in_place",
        ])
        .assert()
        .success();
    p
}

#[test]
fn history_lists_finished_tasks_with_their_commits_and_files() {
    let p = project_with_two_runs();

    let table = stdout_of(&p, &["history"]);
    assert!(table.contains("Write the greeting module"), "{table}");
    assert!(table.lines().next().unwrap().contains("commits"), "{table}");
    // #2 only planned: it is not finished.
    assert!(!table.contains("Fix typo"), "{table}");

    let list = p.json(&["history"]);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    let h = &list[0];
    assert_eq!(h["number"], 1);
    assert_eq!(h["task"]["status"], "ready");
    assert_eq!(h["runs"].as_array().unwrap().len(), 1);
    assert_eq!(h["runs"][0]["state"], "finished");
    assert!(h["totals"]["commits"].as_u64().unwrap() >= 1, "{h}");
    assert!(h["totals"]["usage"]["input_tokens"].as_u64().unwrap() > 0);
    let files: Vec<&str> = h["changed_files"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"hello.txt"), "{h}");
    assert!(h["cost"].is_null(), "no pricing, no cost");

    let one = p.json(&["history", "1"]);
    assert_eq!(one["number"], 1);
    assert_eq!(one["changed_files"], h["changed_files"]);
    let detail = stdout_of(&p, &["history", "1"]);
    for part in [
        "Runs (1)",
        "finished",
        "Commits (",
        "complete subtask 1",
        "hello.txt",
        "QA round 1: approved",
    ] {
        assert!(detail.contains(part), "missing {part:?} in:\n{detail}");
    }
    // Any task can be detailed, finished or not.
    assert!(stdout_of(&p, &["history", "2"]).contains("Fix typo"));
}

#[test]
fn trace_lists_calls_and_reads_full_outputs() {
    let p = project_with_two_runs();

    let text = stdout_of(&p, &["trace", "1"]);
    assert!(text.contains("write_file"), "{text}");
    assert!(text.contains("\"path\": \"hello.txt\""), "{text}");
    assert!(text.contains("files written: hello.txt"), "{text}");
    let call_ids: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split("[call ").nth(1))
        .map(|rest| &rest[..12])
        .collect();
    assert!(!call_ids.is_empty(), "{text}");
    assert!(
        call_ids
            .iter()
            .all(|id| id.bytes().all(|b| b.is_ascii_hexdigit())),
        "{call_ids:?}"
    );

    // `--json` is a `RunTrace`.
    let value = p.json(&["trace", "1"]);
    let trace: vibe_pipeline::RunTrace = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&trace).unwrap(), value);
    let write = trace.calls.iter().find(|c| c.tool == "write_file").unwrap();
    assert_eq!(trace.files_written, ["hello.txt"]);

    // `--full` reads the trace store; the default shows the logged preview.
    let file = write.output_file.clone().expect("output traced");
    std::fs::write(&file, "SENTINEL OUTPUT").unwrap();
    assert!(stdout_of(&p, &["trace", "1", "--full"]).contains("SENTINEL OUTPUT"));
    assert!(!stdout_of(&p, &["trace", "1"]).contains("SENTINEL OUTPUT"));
    std::fs::remove_file(&file).unwrap();
    assert!(stdout_of(&p, &["trace", "1", "--full"]).contains("output file missing"));

    // Filters and run selection.
    let only = p.json(&["trace", "1", "--tool", "write_file"]);
    let calls = only["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    let run = trace.run.to_string();
    let by_prefix = p.json(&["trace", "1", "--run", &run[..8]]);
    assert_eq!(by_prefix["run"], run.as_str());
    let all = p.json(&["trace", "1", "--all"]);
    assert_eq!(all.as_array().unwrap().len(), 1);
    p.vibe()
        .args(["trace", "1", "--run", "zzzz"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("no run of this task matches"));

    // A call whose result is not logged (interrupted run) has no result,
    // whatever the trace store holds.
    let log_path = p.task_dir(1).join("events.jsonl");
    let log = std::fs::read_to_string(&log_path).unwrap();
    let kept: String = log
        .lines()
        .filter(|l| !l.contains("\"tool_returned\""))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&log_path, kept).unwrap();
    let full = stdout_of(&p, &["trace", "1", "--full"]);
    assert!(
        full.contains("no result: the call never returned"),
        "{full}"
    );
    assert!(!full.contains("output not traced"), "{full}");
}

#[test]
fn global_events_are_tagged_filtered_and_ordered() {
    let p = project_with_two_runs();

    let lines = json_lines(&p, &["events"]);
    assert!(
        lines
            .iter()
            .all(|l| l["task"].is_string() && l["schema"] == 2)
    );
    let numbers: std::collections::BTreeSet<u64> = lines
        .iter()
        .map(|l| l["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers.into_iter().collect::<Vec<_>>(), [1, 2]);
    let times: Vec<&str> = lines.iter().map(|l| l["at"].as_str().unwrap()).collect();
    let parsed: Vec<chrono::DateTime<chrono::Utc>> =
        times.iter().map(|t| t.parse().unwrap()).collect();
    assert!(parsed.windows(2).all(|w| w[0] <= w[1]), "time order");

    let finished = json_lines(&p, &["events", "--type", "run_finished"]);
    assert_eq!(finished.len(), 2);
    assert!(
        finished
            .iter()
            .all(|l| l["event"]["type"] == "run_finished")
    );

    let second = json_lines(&p, &["events", "--task", "2"]);
    assert!(!second.is_empty());
    assert!(second.iter().all(|l| l["number"] == 2));

    assert_eq!(
        json_lines(&p, &["events", "--since", "1h"]).len(),
        lines.len()
    );
    assert!(json_lines(&p, &["events", "--since", "2999-01-01T00:00:00Z"]).is_empty());

    let text = stdout_of(&p, &["events"]);
    assert!(text.lines().any(|l| l.starts_with("#1 ")), "{text}");
    assert!(text.lines().any(|l| l.starts_with("#2 ")), "{text}");

    // `--task` and `REF` that do not overlap: nothing.
    assert!(stdout_of(&p, &["events", "1", "--task", "2"]).is_empty());

    // One task: the envelopes as before, `--type` applies.
    let one = json_lines(&p, &["events", "1", "--type", "run_finished"]);
    assert_eq!(one.len(), 1);
    assert!(one[0].get("task").is_none());
    p.vibe()
        .args(["events", "--type", "run_done"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run_done"));
}

/// Wait for `child` at most `secs` seconds. Used by the unix-only `--follow` tests.
#[cfg(unix)]
fn wait_for(child: &mut std::process::Child, secs: u64) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("vibe did not stop within {secs} s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn following_a_resumed_run_waits_for_its_new_end() {
    let p = Project::new();
    p.init();
    p.add_task("Fix typo in README", "teh -> the");
    p.vibe()
        .args([
            "run",
            "1",
            "--provider",
            "mock",
            "--workspace",
            "in_place",
            "--until",
            "plan",
        ])
        .assert()
        .success();
    p.vibe()
        .args([
            "run",
            "1",
            "--provider",
            "mock",
            "--workspace",
            "in_place",
            "--resume",
        ])
        .assert()
        .success();
    // Cut the log right after the resume started: the run is in progress
    // again, after the `run_finished` of its first part.
    let log_path = p.task_dir(1).join("events.jsonl");
    let log = std::fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("\"run_started\""))
        .collect();
    assert_eq!(starts.len(), 2, "one run and its resume");
    let run_of = |l: &str| serde_json::from_str::<Value>(l).unwrap()["event"]["run"].clone();
    assert_eq!(
        run_of(lines[starts[0]]),
        run_of(lines[starts[1]]),
        "a resume keeps the run id"
    );
    let cut = starts[1] + 1;
    assert!(lines[..cut].iter().any(|l| l.contains("\"run_finished\"")));
    std::fs::write(&log_path, lines[..cut].join("\n") + "\n").unwrap();

    let out_path = p.root().join("follow.out");
    let mut child = p
        .vibe_process()
        .args(["events", "1", "--follow"])
        .stdout(std::fs::File::create(&out_path).unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "stopped at the pause's run_finished"
    );
    let rest: String = lines[cut..].iter().map(|l| format!("{l}\n")).collect();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .unwrap()
        .write_all(rest.as_bytes())
        .unwrap();
    assert!(wait_for(&mut child, 10).success());
    let out = std::fs::read_to_string(&out_path).unwrap();
    assert_eq!(out.matches("run finished").count(), 2, "{out}");
}

#[test]
fn a_closed_standard_output_is_not_a_crash() {
    let p = project_with_two_runs();
    let cases: [&[&str]; 7] = [
        &["--json", "history"],
        &["--json", "history", "1"],
        &["--json", "trace", "1"],
        &["trace", "1"],
        &["events", "1"],
        &["--json", "events", "1"],
        &["events"],
    ];
    for args in cases {
        let mut child = p
            .vibe_process()
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Closed before vibe writes anything.
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "vibe {args:?}: {:?}\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn following_every_task_survives_an_unreadable_log_and_sees_new_tasks() {
    let p = project_with_two_runs();
    let broken = p.task_dir(2).join("events.jsonl");
    std::fs::remove_file(&broken).unwrap();
    std::fs::create_dir(&broken).unwrap();

    let out_path = p.root().join("follow.out");
    let err_path = p.root().join("follow.err");
    let mut child = p
        .vibe_process()
        .args(["events", "--follow"])
        .stdout(std::fs::File::create(&out_path).unwrap())
        .stderr(std::fs::File::create(&err_path).unwrap())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    assert!(
        child.try_wait().unwrap().is_none(),
        "stopped: {}",
        std::fs::read_to_string(&err_path).unwrap()
    );
    p.add_task("A third task", "created while following");
    p.vibe()
        .args([
            "run",
            "3",
            "--provider",
            "mock",
            "--dry-run",
            "--workspace",
            "in_place",
        ])
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let killed = StdCommand::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    assert_eq!(wait_for(&mut child, 5).code(), Some(0));

    let out = std::fs::read_to_string(&out_path).unwrap();
    assert!(out.contains("#3 ▶ run"), "{out}");
    assert!(
        !out.lines().any(|l| l.starts_with("#1 ")),
        "from now on only:\n{out}"
    );
    let err = std::fs::read_to_string(&err_path).unwrap();
    assert_eq!(
        err.matches("cannot read the events of task").count(),
        1,
        "{err}"
    );
}

#[test]
fn history_outside_git_uses_the_trace() {
    let p = Project::plain();
    p.init();
    p.add_task(
        "Write the greeting module for the demo application",
        "Create hello.txt containing a friendly greeting so the demo can print it.",
    );
    let script = p.write_script("script.json", &greeting_script());
    p.vibe()
        .args(["run", "1", "--workspace", "in_place", "--script"])
        .arg(&script)
        .assert()
        .success();
    let list = p.json(&["history"]);
    let h = &list.as_array().unwrap()[0];
    assert_eq!(h["totals"]["commits"], 0);
    assert_eq!(h["changed_files"]["source"]["kind"], "trace");
    assert_eq!(h["changed_files"]["files"][0]["path"], "hello.txt");
    let table = stdout_of(&p, &["history"]);
    assert!(table.contains("Write the greeting module"), "{table}");
    assert!(stdout_of(&p, &["history", "1"]).contains("from files written by the agents"));
}
