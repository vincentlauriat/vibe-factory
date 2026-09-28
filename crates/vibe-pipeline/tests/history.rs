//! Task history rebuilt from event logs, `run.json` and git.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use serde_json::json;
use vibe_core::config::ModelPrice;
use vibe_core::{
    AgentRole, ApprovalGate, CallId, Envelope, Event, Phase, RunId, SubtaskId, Task, TaskStatus,
    TaskStore, Usage, VibeConfig,
};
use vibe_pipeline::history::{
    ChangedFilesSource, FileStatus, HistoryFilter, cost_of, parse_name_status, project_history,
    summarize_runs, task_history,
};
use vibe_pipeline::store::EVENTS_FILE;
use vibe_pipeline::{FileTaskStore, PipelineStore, RunState};

fn at(second: i64) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + Duration::days(20_000) + Duration::seconds(second)
}

fn env(second: i64, event: Event) -> Envelope {
    let mut e = Envelope::now(event);
    e.at = at(second);
    e
}

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        ..Usage::default()
    }
}

fn finished(run: RunId, second: i64, status: TaskStatus, total: Usage, active_ms: u64) -> Envelope {
    env(
        second,
        Event::RunFinished {
            run,
            success: matches!(status, TaskStatus::Ready | TaskStatus::Done),
            status,
            usage: total,
            active_ms,
            started_at: at(0),
        },
    )
}

/// A log line as written before 0.5: without the fields 0.5 added.
fn old_line(e: &Envelope) -> String {
    let added: &[&str] = match e.event.type_name() {
        "agent_started" => &["model"],
        "tool_called" => &["call", "subtask"],
        "tool_returned" => &[
            "call",
            "subtask",
            "exit_code",
            "timed_out",
            "output_chars",
            "output_file",
        ],
        "run_finished" => &["usage", "active_ms", "started_at"],
        _ => &[],
    };
    let mut value = serde_json::to_value(e).unwrap();
    let event = value["event"].as_object_mut().unwrap();
    for field in added {
        event.remove(*field);
    }
    format!("{value}\n")
}

fn new_lines(events: &[Envelope]) -> String {
    events
        .iter()
        .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
        .collect()
}

#[test]
fn a_resumed_run_takes_its_totals_from_its_last_run_finished() {
    let run = RunId::new();
    let task = vibe_core::TaskId::new();
    let subtask = SubtaskId::new();
    let events = vec![
        env(0, Event::RunStarted { run, task }),
        env(
            1,
            Event::PhaseStarted {
                run,
                phase: Phase::Build,
            },
        ),
        env(
            2,
            Event::Committed {
                run,
                subtask: Some(subtask),
                commit: "c1".into(),
                message: "add a".into(),
                files: vec!["a.rs".into()],
            },
        ),
        // Old-style integration of the same commit: not counted twice.
        env(
            3,
            Event::SubtaskIntegrated {
                run,
                subtask,
                commit: Some("c1".into()),
                conflicts: vec![],
            },
        ),
        env(
            4,
            Event::PhaseFinished {
                run,
                phase: Phase::Build,
                success: true,
                summary: "1/1".into(),
            },
        ),
        env(
            5,
            Event::ApprovalRequested {
                run,
                gate: ApprovalGate::Merge,
            },
        ),
        finished(run, 6, TaskStatus::Review, usage(10, 5), 1_000),
        env(
            60,
            Event::ApprovalResolved {
                run,
                gate: ApprovalGate::Merge,
                approved: true,
                comment: "ok".into(),
            },
        ),
        env(61, Event::RunStarted { run, task }),
        env(
            62,
            Event::ValidationFinished {
                run,
                command: "cargo test".into(),
                integration: false,
                passed: false,
                exit_code: Some(1),
            },
        ),
        env(
            63,
            Event::ValidationFinished {
                run,
                command: "cargo test".into(),
                integration: false,
                passed: true,
                exit_code: Some(0),
            },
        ),
        env(
            64,
            Event::Merged {
                run,
                commit: "m1".into(),
                branch: "vibe/x".into(),
                base: "main".into(),
            },
        ),
        finished(run, 65, TaskStatus::Done, usage(30, 9), 2_500),
    ];
    let runs = summarize_runs(&events);
    assert_eq!(runs.len(), 1);
    let r = &runs[0];
    assert_eq!(r.resumes, 1);
    assert_eq!(r.started_at, at(0));
    assert_eq!(r.finished_at, Some(at(65)));
    assert_eq!(r.status, Some(TaskStatus::Done));
    assert!(r.totals_known);
    assert_eq!((r.usage, r.active_ms), (usage(30, 9), 2_500));
    assert_eq!(r.phases.len(), 1);
    assert_eq!(r.phases[0].success, Some(true));
    assert_eq!(r.commits.len(), 1);
    assert_eq!(r.commits[0].subtask, Some(subtask));
    assert_eq!(r.merged.as_ref().unwrap().commit, "m1");
    assert_eq!(r.validations.len(), 2);
    assert_eq!(r.approvals.len(), 1);
    assert_eq!(r.pending_approval, None);
    assert_eq!(r.last_event_at, at(65));
}

#[test]
fn old_logs_have_unknown_totals_unless_run_json_describes_the_run() {
    let (first, second) = (RunId::new(), RunId::new());
    let task = vibe_core::TaskId::new();
    let text: String = [
        env(0, Event::RunStarted { run: first, task }),
        env(
            1,
            Event::PhaseFinished {
                run: first,
                phase: Phase::Build,
                success: false,
                summary: "no subtask done".into(),
            },
        ),
        finished(first, 2, TaskStatus::Failed, usage(1, 1), 1),
        env(10, Event::RunStarted { run: second, task }),
        finished(second, 11, TaskStatus::Ready, usage(1, 1), 1),
    ]
    .iter()
    .map(old_line)
    .collect();
    let events = vibe_pipeline::parse_event_log(&text);
    assert_eq!(events.len(), 5);
    let mut runs = summarize_runs(&events);
    assert!(
        runs.iter()
            .all(|r| !r.totals_known && r.usage == Usage::default())
    );
    assert_eq!(
        runs[0].last_error.as_deref(),
        Some("build failed: no subtask done")
    );
    assert_eq!(runs[1].last_error, None);

    let mut state = RunState::new(second, task, Phase::Merge);
    state.usage = usage(100, 50);
    state.active_ms = 9_000;
    for r in &mut runs {
        r.apply_run_state(&state);
    }
    assert!(!runs[0].totals_known);
    assert!(runs[1].totals_known);
    assert_eq!((runs[1].usage, runs[1].active_ms), (usage(100, 50), 9_000));
}

fn priced() -> VibeConfig {
    let mut config = VibeConfig::default();
    config.pricing.insert(
        "anthropic/big".into(),
        ModelPrice {
            input: 10.0,
            output: 20.0,
            ..ModelPrice::default()
        },
    );
    config.pricing.insert(
        "anthropic/small".into(),
        ModelPrice {
            input: 1.0,
            output: 2.0,
            ..ModelPrice::default()
        },
    );
    config
}

fn session(run: RunId, role: AgentRole, model: &str, used: Usage) -> [Envelope; 2] {
    [
        env(
            0,
            Event::AgentStarted {
                run,
                role: role.clone(),
                subtask: None,
                model: model.into(),
            },
        ),
        env(
            1,
            Event::AgentFinished {
                run,
                role,
                steps: 1,
                usage: used,
                stop: "end_turn".into(),
            },
        ),
    ]
}

#[test]
fn cost_needs_a_price_for_every_session() {
    let run = RunId::new();
    let million = usage(1_000_000, 1_000_000);
    let mut events: Vec<Envelope> = session(run, AgentRole::Planner, "big", million).into();
    events.extend(session(run, AgentRole::Coder, "small", million));
    let cost = cost_of(&events, &priced()).unwrap();
    assert!((cost.amount - 33.0).abs() < 1e-9);
    assert_eq!(cost.currency, "USD");
    assert!(cost.complete);

    // Totals of the run covered by its sessions: complete; more tokens than
    // the sessions used (a repair call outside any session): a lower bound.
    let mut totalled = events.clone();
    totalled.push(finished(
        run,
        9,
        TaskStatus::Done,
        usage(2_000_000, 2_000_000),
        1,
    ));
    assert!(cost_of(&totalled, &priced()).unwrap().complete);
    let mut outside = events.clone();
    outside.push(finished(
        run,
        9,
        TaskStatus::Done,
        usage(2_000_100, 2_000_000),
        1,
    ));
    let lower_bound = cost_of(&outside, &priced()).unwrap();
    assert!(!lower_bound.complete);
    assert!((lower_bound.amount - 33.0).abs() < 1e-9);

    // No pricing table: no cost at all.
    assert_eq!(cost_of(&events, &VibeConfig::default()), None);

    // A session that never finished: a lower bound.
    let mut crashed = events.clone();
    crashed.push(session(run, AgentRole::QaReviewer, "big", million)[0].clone());
    assert!(!cost_of(&crashed, &priced()).unwrap().complete);

    // A model without a price, or a session logged before 0.5: no cost.
    let mut unpriced = events.clone();
    unpriced.extend(session(run, AgentRole::QaReviewer, "other", million));
    assert_eq!(cost_of(&unpriced, &priced()), None);
    let old: String = events.iter().map(old_line).collect();
    assert_eq!(
        cost_of(&vibe_pipeline::parse_event_log(&old), &priced()),
        None
    );
}

#[test]
fn name_status_output_is_parsed_with_renames() {
    let files =
        parse_name_status("M\0src/a.rs\0R087\0old name.rs\0new name.rs\0A\0b.rs\0D\0c.rs\0");
    let summary: Vec<(&str, FileStatus, Option<&str>)> = files
        .iter()
        .map(|f| (f.path.as_str(), f.status, f.old_path.as_deref()))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("b.rs", FileStatus::Added, None),
            ("c.rs", FileStatus::Deleted, None),
            ("new name.rs", FileStatus::Renamed, Some("old name.rs")),
            ("src/a.rs", FileStatus::Modified, None),
        ]
    );
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(root: &Path, files: &[(&str, &str)], message: &str) -> String {
    for (path, content) in files {
        std::fs::write(root.join(path), content).unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}

async fn task_with_log(
    store: &FileTaskStore,
    title: &str,
    status: TaskStatus,
    branch: Option<&str>,
    log: String,
) -> Task {
    let mut task = Task::new(title, "");
    task.status = status;
    task.updated_at = at(0);
    task.branch = branch.map(str::to_string);
    store.save_task(&task).await.unwrap();
    let dir = store.task_dir(task.id).await.unwrap();
    std::fs::write(dir.join(EVENTS_FILE), log).unwrap();
    task
}

fn committed(run: RunId, second: i64, commit: &str, files: &[&str]) -> Envelope {
    env(
        second,
        Event::Committed {
            run,
            subtask: None,
            commit: commit.into(),
            message: "work".into(),
            files: files.iter().map(|f| f.to_string()).collect(),
        },
    )
}

fn merged(run: RunId, second: i64, commit: &str, branch: &str) -> Envelope {
    env(
        second,
        Event::Merged {
            run,
            commit: commit.into(),
            branch: branch.into(),
            base: "main".into(),
        },
    )
}

#[tokio::test]
async fn changed_files_come_from_the_most_exact_source_available() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("commit.gpgsign", "false"),
    ] {
        git(root, &["config", k, v]);
    }
    commit(root, &[("readme.md", "hello"), ("old.txt", "x")], "initial");
    let store = FileTaskStore::open(root).unwrap();

    // Ready, branch still there, with its base recorded by the worktree
    // provider.
    git(root, &["checkout", "-q", "-b", "vibe/ready"]);
    commit(
        root,
        &[("ready.rs", "r"), ("readme.md", "changed")],
        "ready",
    );
    git(root, &["checkout", "-q", "main"]);
    git(root, &["config", "branch.vibe/ready.vibebase", "main"]);
    let run = RunId::new();
    let ready = task_with_log(
        &store,
        "Ready",
        TaskStatus::Ready,
        Some("vibe/ready"),
        new_lines(&[finished(run, 10, TaskStatus::Ready, usage(1, 1), 1)]),
    )
    .await;

    // Done by fast-forward, over two commits; the branch is kept, and now
    // diffs as empty against its base.
    git(root, &["checkout", "-q", "-b", "vibe/ff"]);
    let ff1 = commit(root, &[("ff1.rs", "1")], "ff 1");
    let ff2 = commit(root, &[("ff2.rs", "2")], "ff 2");
    git(root, &["checkout", "-q", "main"]);
    git(root, &["merge", "-q", "--ff-only", "vibe/ff"]);
    let run = RunId::new();
    let ff = task_with_log(
        &store,
        "Fast forward",
        TaskStatus::Done,
        Some("vibe/ff"),
        new_lines(&[
            committed(run, 20, &ff1, &["ff1.rs"]),
            committed(run, 21, &ff2, &["ff2.rs"]),
            merged(run, 22, &ff2, "vibe/ff"),
            finished(run, 23, TaskStatus::Done, usage(1, 1), 1),
        ]),
    )
    .await;

    // Done by a merge commit.
    git(root, &["checkout", "-q", "-b", "vibe/nff"]);
    commit(root, &[("nff.rs", "n")], "nff");
    git(root, &["rm", "-q", "old.txt"]);
    git(root, &["commit", "-q", "-m", "remove old"]);
    git(root, &["checkout", "-q", "main"]);
    commit(root, &[("main.rs", "m")], "meanwhile on main");
    git(
        root,
        &["merge", "-q", "--no-ff", "-m", "merge nff", "vibe/nff"],
    );
    let merge = git(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "-q", "-D", "vibe/nff"]);
    let run = RunId::new();
    let no_ff = task_with_log(
        &store,
        "Merge commit",
        TaskStatus::Done,
        None,
        new_lines(&[
            merged(run, 30, &merge, "vibe/nff"),
            finished(run, 31, TaskStatus::Done, usage(1, 1), 1),
        ]),
    )
    .await;

    // Done in place: only the trace knows. The merge record points to a
    // commit git does not have, which is reported, not fatal.
    let run = RunId::new();
    let call = CallId::new();
    let in_place = task_with_log(
        &store,
        "In place",
        TaskStatus::Done,
        None,
        new_lines(&[
            env(
                40,
                Event::ToolCalled {
                    run,
                    role: AgentRole::Coder,
                    tool: "edit_file".into(),
                    input: json!({"path": "src/lib.rs"}),
                    call,
                    subtask: None,
                },
            ),
            env(
                41,
                Event::ToolReturned {
                    run,
                    role: AgentRole::Coder,
                    tool: "edit_file".into(),
                    is_error: false,
                    duration_ms: 1,
                    preview: String::new(),
                    call,
                    subtask: None,
                    exit_code: None,
                    timed_out: false,
                    output_chars: 0,
                    output_file: None,
                },
            ),
            merged(run, 42, "0123456789abcdef0123456789abcdef01234567", "x"),
            finished(run, 43, TaskStatus::Done, usage(1, 1), 1),
        ]),
    )
    .await;

    // Ready, but its branch was merged outside vibe: the empty branch diff
    // gives way to the commit events.
    git(root, &["branch", "vibe/outside"]);
    let run = RunId::new();
    let outside = task_with_log(
        &store,
        "Outside",
        TaskStatus::Ready,
        Some("vibe/outside"),
        new_lines(&[
            committed(run, 5, "def", &["x.rs"]),
            finished(run, 6, TaskStatus::Ready, usage(1, 1), 1),
        ]),
    )
    .await;

    // Failed: only with `all`.
    let run = RunId::new();
    let failed = task_with_log(
        &store,
        "Failed",
        TaskStatus::Failed,
        None,
        new_lines(&[
            committed(run, 50, "abc", &["f.rs", "g.rs"]),
            finished(run, 51, TaskStatus::Failed, usage(1, 1), 1),
        ]),
    )
    .await;
    // Not finished: never listed.
    task_with_log(
        &store,
        "Building",
        TaskStatus::Building,
        None,
        String::new(),
    )
    .await;

    let config = VibeConfig::default();
    let branch_of = |t: &Task| t.branch.clone();
    let history = project_history(&store, root, &config, HistoryFilter::default(), &branch_of)
        .await
        .unwrap();
    let ids: Vec<_> = history.iter().map(|h| h.task.id).collect();
    assert_eq!(
        ids,
        vec![in_place.id, no_ff.id, ff.id, ready.id, outside.id]
    );

    let files = |i: usize| -> Vec<(String, FileStatus)> {
        history[i]
            .changed_files
            .files
            .iter()
            .map(|f| (f.path.clone(), f.status))
            .collect()
    };
    assert_eq!(history[4].changed_files.source, ChangedFilesSource::Commits);
    assert_eq!(files(4), vec![("x.rs".into(), FileStatus::Unknown)]);
    assert_eq!(
        history[3].changed_files.source,
        ChangedFilesSource::Branch {
            branch: "vibe/ready".into(),
            base: "main".into()
        }
    );
    assert_eq!(
        files(3),
        vec![
            ("readme.md".into(), FileStatus::Modified),
            ("ready.rs".into(), FileStatus::Added)
        ]
    );
    assert_eq!(
        history[2].changed_files.source,
        ChangedFilesSource::MergeCommit {
            commit: ff2,
            fast_forward: true
        }
    );
    assert_eq!(
        files(2),
        vec![
            ("ff1.rs".into(), FileStatus::Added),
            ("ff2.rs".into(), FileStatus::Added)
        ]
    );
    assert_eq!(history[2].totals.commits, 2);
    assert_eq!(
        history[1].changed_files.source,
        ChangedFilesSource::MergeCommit {
            commit: merge,
            fast_forward: false
        }
    );
    assert_eq!(
        files(1),
        vec![
            ("nff.rs".into(), FileStatus::Added),
            ("old.txt".into(), FileStatus::Deleted)
        ]
    );
    assert_eq!(history[0].changed_files.source, ChangedFilesSource::Trace);
    assert!(history[0].changed_files.approximate);
    assert_eq!(files(0), vec![("src/lib.rs".into(), FileStatus::Unknown)]);
    assert_eq!(history[0].errors.len(), 1, "{:?}", history[0].errors);
    assert!(history.iter().skip(1).all(|h| h.errors.is_empty()));
    assert!(
        history
            .iter()
            .all(|h| h.totals.complete && h.totals.runs == 1)
    );

    let all = project_history(
        &store,
        root,
        &config,
        HistoryFilter { all: true },
        &branch_of,
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 6);
    let failed = all.iter().find(|h| h.task.id == failed.id).unwrap();
    assert_eq!(failed.changed_files.source, ChangedFilesSource::Commits);
    assert_eq!(failed.changed_files.files.len(), 2);

    // A base that does not exist: the error is kept and the next source is
    // used.
    git(root, &["config", "branch.vibe/ready.vibebase", "gone"]);
    let one = task_history(&store, root, &config, ready.clone(), Some("vibe/ready"))
        .await
        .unwrap();
    assert_eq!(one.changed_files.source, ChangedFilesSource::None);
    assert_eq!(one.errors.len(), 1);
    assert!(one.errors[0].contains("gone"), "{}", one.errors[0]);
}

#[tokio::test]
async fn history_outside_git_uses_the_log_and_run_json() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileTaskStore::open(dir.path()).unwrap();
    let run = RunId::new();
    let text: String = [finished(run, 5, TaskStatus::Ready, usage(1, 1), 1)]
        .iter()
        .map(old_line)
        .collect();
    let task = task_with_log(&store, "Old", TaskStatus::Ready, None, text).await;
    let mut state = RunState::new(run, task.id, Phase::Merge);
    state.usage = usage(7, 3);
    store.save_run_state(&state).await.unwrap();
    let h = task_history(&store, dir.path(), &priced(), task, None)
        .await
        .unwrap();
    assert_eq!(h.number, 1);
    assert_eq!(h.totals.usage, usage(7, 3));
    assert!(h.totals.complete);
    assert_eq!(h.changed_files.source, ChangedFilesSource::None);
    assert!(h.errors.is_empty());
    assert_eq!(h.last_activity, at(5));
    // No agent session: nothing to price.
    assert_eq!(h.cost.map(|c| c.amount), Some(0.0));
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("commit.gpgsign", "false"),
    ] {
        git(root, &["config", k, v]);
    }
}

#[tokio::test]
async fn a_fast_forward_to_an_integration_merge_lists_every_subtask() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_repo(root);
    commit(root, &[("readme.md", "hello")], "initial");
    let store = FileTaskStore::open(root).unwrap();

    // The task branch integrates a subtask with a merge commit, then the
    // final merge fast-forwards main to that merge commit.
    git(root, &["checkout", "-q", "-b", "vibe/t"]);
    git(root, &["checkout", "-q", "-b", "vibe/t-sub"]);
    let sub = commit(root, &[("sub.rs", "s")], "subtask");
    git(root, &["checkout", "-q", "vibe/t"]);
    let other = commit(root, &[("other.rs", "o")], "other subtask");
    git(
        root,
        &["merge", "-q", "--no-ff", "-m", "integrate", "vibe/t-sub"],
    );
    let integration = git(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", "main"]);
    git(root, &["merge", "-q", "--ff-only", "vibe/t"]);
    let run = RunId::new();
    let task = task_with_log(
        &store,
        "Integrated",
        TaskStatus::Done,
        None,
        new_lines(&[
            committed(run, 1, &sub, &["sub.rs"]),
            committed(run, 2, &other, &["other.rs"]),
            committed(run, 3, &integration, &["sub.rs"]),
            merged(run, 4, &integration, "vibe/t"),
        ]),
    )
    .await;
    let h = task_history(&store, root, &VibeConfig::default(), task, None)
        .await
        .unwrap();
    assert_eq!(
        h.changed_files.source,
        ChangedFilesSource::MergeCommit {
            commit: integration,
            fast_forward: true
        }
    );
    assert!(!h.changed_files.approximate);
    let paths: Vec<&str> = h
        .changed_files
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(paths, vec!["other.rs", "sub.rs"]);
}

#[tokio::test]
async fn commits_lost_to_a_reset_do_not_widen_the_merge_diff() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_repo(root);
    commit(root, &[("readme.md", "hello")], "initial");
    let store = FileTaskStore::open(root).unwrap();

    // First run: a commit on the old base, then thrown away.
    git(root, &["checkout", "-q", "-b", "vibe/r"]);
    let old = commit(root, &[("old.rs", "o")], "first attempt");
    git(root, &["checkout", "-q", "main"]);
    commit(root, &[("base.rs", "b")], "main moves on");
    git(root, &["checkout", "-q", "vibe/r"]);
    git(root, &["reset", "-q", "--hard", "main"]);
    let new = commit(root, &[("new.rs", "n")], "second attempt");
    git(root, &["checkout", "-q", "main"]);
    git(root, &["merge", "-q", "--ff-only", "vibe/r"]);
    let (first, second) = (RunId::new(), RunId::new());
    let task = task_with_log(
        &store,
        "Reset",
        TaskStatus::Done,
        None,
        new_lines(&[
            committed(first, 1, &old, &["old.rs"]),
            committed(second, 2, &new, &["new.rs"]),
            merged(second, 3, &new, "vibe/r"),
        ]),
    )
    .await;
    let config = VibeConfig::default();
    let h = task_history(&store, root, &config, task, None)
        .await
        .unwrap();
    let paths: Vec<&str> = h
        .changed_files
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(paths, vec!["new.rs"]);
    assert!(!h.changed_files.approximate);

    // No recorded commit reaches the merge: only the merged commit itself
    // is known, and the list says it may be incomplete.
    let task = task_with_log(
        &store,
        "Lost",
        TaskStatus::Done,
        None,
        new_lines(&[
            committed(first, 1, &old, &["old.rs"]),
            merged(first, 3, &new, "vibe/r"),
        ]),
    )
    .await;
    let h = task_history(&store, root, &config, task, None)
        .await
        .unwrap();
    let paths: Vec<&str> = h
        .changed_files
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(paths, vec!["new.rs"]);
    assert!(h.changed_files.approximate);
}

#[tokio::test]
async fn one_unreadable_task_does_not_fail_the_project_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileTaskStore::open(dir.path()).unwrap();
    let run = RunId::new();
    let log = new_lines(&[finished(run, 5, TaskStatus::Done, usage(1, 1), 1)]);
    let good = task_with_log(&store, "Good", TaskStatus::Done, None, log.clone()).await;
    let bad = task_with_log(&store, "Bad", TaskStatus::Done, None, log).await;
    let bad_dir = store.task_dir(bad.id).await.unwrap();
    std::fs::write(bad_dir.join("run.json"), "{ not json").unwrap();
    std::fs::write(bad_dir.join("qa_report_1.json"), "{ not json").unwrap();
    let branch_of = |_: &Task| None;
    let history = project_history(
        &store,
        dir.path(),
        &VibeConfig::default(),
        HistoryFilter::default(),
        &branch_of,
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 2);
    let bad = history.iter().find(|h| h.task.id == bad.id).unwrap();
    assert_eq!(bad.errors.len(), 2, "{:?}", bad.errors);
    assert_eq!(bad.runs.len(), 1);
    let good = history.iter().find(|h| h.task.id == good.id).unwrap();
    assert!(good.errors.is_empty());
}

#[test]
fn run_json_updates_the_totals_of_a_resumed_run_that_did_not_finish() {
    let run = RunId::new();
    let task = vibe_core::TaskId::new();
    let events = vec![
        env(0, Event::RunStarted { run, task }),
        finished(run, 1, TaskStatus::Review, usage(10, 5), 1_000),
        env(2, Event::RunStarted { run, task }),
    ];
    let mut runs = summarize_runs(&events);
    let mut state = RunState::new(run, task, Phase::Build);
    state.usage = usage(30, 9);
    state.active_ms = 4_000;
    runs[0].apply_run_state(&state);
    assert_eq!((runs[0].usage, runs[0].active_ms), (usage(30, 9), 4_000));

    // A finished run keeps the totals of its `run_finished`.
    let mut done = events.clone();
    done.push(finished(run, 3, TaskStatus::Done, usage(40, 10), 5_000));
    let mut runs = summarize_runs(&done);
    runs[0].apply_run_state(&state);
    assert_eq!((runs[0].usage, runs[0].active_ms), (usage(40, 10), 5_000));
}

#[test]
fn a_session_lost_to_a_crash_is_not_paired_after_the_resume() {
    let run = RunId::new();
    let task = vibe_core::TaskId::new();
    let million = usage(1_000_000, 1_000_000);
    let crashed = session(run, AgentRole::Coder, "big", million)[0].clone();
    let mut events = vec![env(0, Event::RunStarted { run, task }), crashed];
    events.push(env(5, Event::RunStarted { run, task }));
    events.extend(session(run, AgentRole::Coder, "small", million));
    let cost = cost_of(&events, &priced()).unwrap();
    assert!((cost.amount - 3.0).abs() < 1e-9, "{}", cost.amount);
    assert!(!cost.complete);
}

#[tokio::test]
async fn runs_without_run_finished_are_running_or_interrupted() {
    use vibe_pipeline::history::RunSummaryState;
    let dir = tempfile::tempdir().unwrap();
    let store = FileTaskStore::open(dir.path()).unwrap();
    let (crashed, done, current) = (RunId::new(), RunId::new(), RunId::new());
    let id = vibe_core::TaskId::new();
    let log = new_lines(&[
        env(
            0,
            Event::RunStarted {
                run: crashed,
                task: id,
            },
        ),
        env(
            1,
            Event::RunStarted {
                run: done,
                task: id,
            },
        ),
        finished(done, 2, TaskStatus::Review, usage(1, 1), 1),
        env(
            3,
            Event::RunStarted {
                run: current,
                task: id,
            },
        ),
    ]);
    let task = task_with_log(&store, "Runs", TaskStatus::Building, None, log).await;
    let states = |h: &vibe_pipeline::TaskHistory| -> Vec<RunSummaryState> {
        h.runs.iter().map(|r| r.state).collect()
    };
    let config = VibeConfig::default();

    let lock = store.lock_run(task.id).await.unwrap();
    let h = task_history(&store, dir.path(), &config, task.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        states(&h),
        vec![
            RunSummaryState::Interrupted,
            RunSummaryState::Finished,
            RunSummaryState::Running
        ]
    );
    drop(lock);
    let h = task_history(&store, dir.path(), &config, task, None)
        .await
        .unwrap();
    assert_eq!(h.runs[2].state, RunSummaryState::Interrupted);
}

#[tokio::test]
async fn revisions_that_look_like_options_are_not_read_as_options() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_repo(root);
    commit(root, &[("readme.md", "hello")], "initial");
    git(root, &["checkout", "-q", "-b", "x"]);
    commit(root, &[("x.rs", "x")], "x");
    git(root, &["checkout", "-q", "main"]);
    let out = tempfile::tempdir().unwrap();
    let base = format!("--output={}", out.path().join("pwned").display());
    git(root, &["config", "branch.x.vibebase", &base]);
    let store = FileTaskStore::open(root).unwrap();
    let task = task_with_log(&store, "X", TaskStatus::Ready, Some("x"), String::new()).await;
    let h = task_history(&store, root, &VibeConfig::default(), task, Some("x"))
        .await
        .unwrap();
    assert_eq!(h.errors.len(), 1, "{:?}", h.errors);
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}
