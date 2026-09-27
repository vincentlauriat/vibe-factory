//! End-to-end pipeline runs driven by a scripted, role-routing provider.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::json;
use tokio::sync::watch;
use vibe_core::{Complexity, Event, MemoryStore, Phase, SubtaskStatus, TaskStatus, TaskStore};
use vibe_pipeline::{MemoryFile, PipelineStore, RunOptions, RunStatus};

fn phases(report: &vibe_pipeline::RunReport) -> Vec<Phase> {
    report.phases.iter().map(|p| p.phase).collect()
}

fn plan_json(phases: serde_json::Value) -> vibe_core::CompletionResponse {
    json(json!({"approach": "small steps", "phases": phases}))
}

#[tokio::test]
async fn trivial_task_skips_spec() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([{"name": "Only", "subtasks": [
            {"title": "Fix the typo", "description": "edit README", "files": ["README.md"],
             "verification": ["grep the"]}
        ]}]))],
    );
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);

    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();

    assert_eq!(report.final_status, TaskStatus::Ready);
    assert!(report.is_success());
    assert_eq!(
        phases(&report),
        vec![
            Phase::Assess,
            Phase::Plan,
            Phase::Build,
            Phase::Qa,
            Phase::Merge
        ]
    );
    assert_eq!(report.task.complexity, Some(Complexity::Trivial));
    assert!(
        h.router.calls_for("assessor").is_empty(),
        "heuristic fast path"
    );
    assert!(h.router.calls_for("gatherer").is_empty());
    let planner = &h.router.calls_for("planner")[0];
    assert!(planner.system.contains("No written specification"));
    assert!(planner.system.contains("teh -> the"));
    assert!(h.store.load_spec(task.id).await.unwrap().is_none());
    assert_eq!(
        h.commits.lock().unwrap().clone(),
        vec!["vibe: complete subtask 1 - Fix the typo".to_string()]
    );
    assert!(report.usage.total() > 0);

    let dir = h.task_dir(&task).await;
    for f in [
        "task.json",
        "plan.json",
        "plan.md",
        "qa_report_1.json",
        "qa_report_1.md",
        "progress.md",
        "run.json",
        "events.jsonl",
    ] {
        assert!(dir.join(f).exists(), "missing {f}");
    }
    let saved = h.store.load_task(task.id).await.unwrap();
    assert_eq!(saved.status, TaskStatus::Ready);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Finished);
    assert_eq!(state.qa_round, 1);
    let log = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(log.contains("\"run_started\"") && log.contains("\"run_finished\""));
    let events = h.collector.events();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::RunFinished {
            success: true,
            status: TaskStatus::Ready,
            ..
        }
    )));
    // The run totals survive in the log, not only in `run.json`.
    let Some(Event::RunFinished {
        usage,
        active_ms,
        started_at,
        ..
    }) = events.last()
    else {
        panic!("the run ends with run_finished");
    };
    assert_eq!(*usage, state.usage);
    assert!(usage.total() > 0);
    assert_eq!(*active_ms, state.active_ms);
    assert_eq!(*started_at, state.started_at);
    // The subtask commit is announced with its subtask.
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    let subtask = plan.subtasks().next().unwrap().id;
    let committed: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Committed { .. }))
        .collect();
    assert_eq!(
        committed,
        vec![&Event::Committed {
            run: report.run_id,
            subtask: Some(subtask),
            commit: "abc123".into(),
            message: "vibe: complete subtask 1 - Fix the typo".into(),
            // The fake committer does not use git.
            files: Vec::new(),
        }]
    );
    // Sessions name the model the resolver picked.
    let default_model = vibe_core::VibeConfig::default().default_model;
    assert!(events.iter().all(|e| match e {
        Event::AgentStarted { model, .. } => default_model.ends_with(&format!("/{model}")),
        _ => true,
    }));
    // In place: no branch to record.
    assert_eq!(saved.branch, None);
}

/// Workspace provider in place whose merge "succeeds" on a named branch.
struct MergingOnBranch;

#[async_trait::async_trait]
impl vibe_core::WorkspaceProvider for MergingOnBranch {
    fn name(&self) -> &str {
        "merging"
    }
    async fn open(
        &self,
        root: &std::path::Path,
        t: &vibe_core::Task,
    ) -> vibe_core::Result<vibe_core::Workspace> {
        let mut ws = vibe_core::InPlaceWorkspace.open(root, t).await?;
        ws.branch = Some("vibe/fix-it-1234".into());
        ws.base_branch = Some("main".into());
        Ok(ws)
    }
    async fn merge(&self, _w: &vibe_core::Workspace) -> vibe_core::Result<vibe_core::MergeOutcome> {
        Ok(vibe_core::MergeOutcome::Merged {
            commit: Some("feedbeef".into()),
        })
    }
    async fn discard(&self, _w: &vibe_core::Workspace) -> vibe_core::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn auto_merge_publishes_merged_and_records_the_branch() {
    let mut h = Harness::new().await;
    h.config.pipeline.auto_merge = true;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "A", "description": "a"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("a")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let store: Arc<dyn PipelineStore> = h.store.clone();
    let pipeline = vibe_pipeline::Pipeline::new(vibe_pipeline::PipelineDeps {
        registry: Arc::new(h.registry.clone()),
        providers: Arc::new(Resolver(h.router.clone())),
        store,
        workspace: Arc::new(MergingOnBranch),
        tools: vibe_core::ToolRegistry::new(),
        events: h.events.clone(),
        config: h.config.clone(),
        project_root: h.root(),
        committer: Some(h.committer()),
        resetter: None,
        subtask_workspaces: None,
    });
    let report = pipeline.run(task.id, RunOptions::default()).await.unwrap();
    assert_eq!(report.final_status, TaskStatus::Done);
    let merged: Vec<Event> = h
        .collector
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::Merged { .. }))
        .collect();
    assert_eq!(
        merged,
        vec![Event::Merged {
            run: report.run_id,
            commit: "feedbeef".into(),
            branch: "vibe/fix-it-1234".into(),
            base: "main".into(),
        }]
    );
    let saved = h.store.load_task(task.id).await.unwrap();
    assert_eq!(saved.branch.as_deref(), Some("vibe/fix-it-1234"));
}

#[tokio::test]
async fn auto_merge_with_nothing_to_merge_publishes_no_merged_event() {
    let mut h = Harness::new().await;
    h.config.pipeline.auto_merge = true;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "A", "description": "a"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("a")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    // In place merges nothing.
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Done);
    assert!(
        !h.collector
            .events()
            .iter()
            .any(|e| matches!(e, Event::Merged { .. }))
    );
}

/// Workspace provider whose `open` always fails.
struct Unopenable;

#[async_trait::async_trait]
impl vibe_core::WorkspaceProvider for Unopenable {
    fn name(&self) -> &str {
        "unopenable"
    }
    async fn open(
        &self,
        _root: &std::path::Path,
        _t: &vibe_core::Task,
    ) -> vibe_core::Result<vibe_core::Workspace> {
        Err(vibe_core::Error::workspace("cannot open"))
    }
    async fn merge(&self, _w: &vibe_core::Workspace) -> vibe_core::Result<vibe_core::MergeOutcome> {
        Ok(vibe_core::MergeOutcome::NoChanges)
    }
    async fn discard(&self, _w: &vibe_core::Workspace) -> vibe_core::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn failed_workspace_open_finishes_the_run_unsuccessfully_with_its_totals() {
    let h = Harness::new().await;
    let mut task = h.task("Fix typo in docs", "").await;
    // A ready task run again from QA must not look successful.
    task.set_status(TaskStatus::Ready);
    h.store.save_task(&task).await.unwrap();
    let mut prev = vibe_pipeline::RunState::new(vibe_core::RunId::new(), task.id, Phase::Qa);
    prev.usage = vibe_core::Usage {
        input_tokens: 7,
        output_tokens: 3,
        ..vibe_core::Usage::default()
    };
    prev.active_ms = 1234;
    prev.status = RunStatus::Paused;
    h.store.save_run_state(&prev).await.unwrap();
    let store: Arc<dyn PipelineStore> = h.store.clone();
    let pipeline = vibe_pipeline::Pipeline::new(vibe_pipeline::PipelineDeps {
        registry: Arc::new(h.registry.clone()),
        providers: Arc::new(Resolver(h.router.clone())),
        store,
        workspace: Arc::new(Unopenable),
        tools: vibe_core::ToolRegistry::new(),
        events: h.events.clone(),
        config: h.config.clone(),
        project_root: h.root(),
        committer: None,
        resetter: None,
        subtask_workspaces: None,
    });
    assert!(pipeline.resume(task.id).await.is_err());
    let finished: Vec<Event> = h
        .collector
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::RunFinished { .. }))
        .collect();
    assert_eq!(
        finished,
        vec![Event::RunFinished {
            run: prev.run_id,
            success: false,
            status: TaskStatus::Ready,
            usage: prev.usage,
            active_ms: 1234,
            started_at: prev.started_at,
        }]
    );
}

#[tokio::test]
async fn a_storage_error_mid_run_still_finishes_the_run() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in docs", "").await;
    // An unreadable spec makes the run fail after `run_started`.
    std::fs::write(h.task_dir(&task).await.join("spec.json"), "{not json").unwrap();
    assert!(
        h.pipeline()
            .run(task.id, RunOptions::default())
            .await
            .is_err()
    );
    let events = h.collector.events();
    assert!(matches!(events.first(), Some(Event::RunStarted { .. })));
    assert!(
        matches!(
            events.last(),
            Some(Event::RunFinished {
                success: false,
                status: TaskStatus::Failed,
                ..
            })
        ),
        "{events:?}"
    );
    let log = std::fs::read_to_string(h.task_dir(&task).await.join("events.jsonl")).unwrap();
    assert!(
        log.trim_end()
            .lines()
            .last()
            .unwrap()
            .contains("\"run_finished\"")
    );
}

/// Coder that calls the `echo` tool once, then reports.
fn echoing_coder(h: &mut Harness) {
    h.tools =
        vibe_core::ToolRegistry::new().with(Arc::new(vibe_agents::test_support::EchoTool::new()));
    h.router.route(
        CODER,
        None,
        vec![
            vibe_agents::test_support::tool_use_response(vec![(
                "t1",
                "echo",
                json!({"text": "traced text"}),
            )]),
            coder_done("a"),
        ],
    );
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "A", "description": "a"}]}
        ]))],
    );
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
}

#[tokio::test]
async fn tool_outputs_are_traced_under_the_task_directory() {
    let mut h = Harness::new().await;
    let task = h.task("Fix typo in docs", "").await;
    echoing_coder(&mut h);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    let dir_name = h
        .task_dir(&task)
        .await
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let events = h.collector.events();
    let (call, file) = events
        .iter()
        .find_map(|e| match e {
            Event::ToolReturned {
                call, output_file, ..
            } => Some((*call, output_file.clone().unwrap())),
            _ => None,
        })
        .expect("a traced tool call");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolCalled { call: c, subtask: Some(_), .. } if *c == call
    )));
    assert_eq!(
        file,
        format!(".vibe/tool-output/{dir_name}/{}/{call}.txt", report.run_id)
    );
    assert_eq!(
        std::fs::read_to_string(h.root().join(&file)).unwrap(),
        "traced text"
    );
}

#[tokio::test]
async fn trace_outputs_off_writes_nothing() {
    let mut h = Harness::new().await;
    h.config.pipeline.trace_outputs = false;
    let task = h.task("Fix typo in docs", "").await;
    echoing_coder(&mut h);
    h.pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    let events = h.collector.events();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolReturned {
            output_file: None,
            ..
        }
    )));
    assert!(!h.root().join(".vibe").join("tool-output").exists());
}

#[tokio::test]
async fn standard_task_full_loop_with_parallel_build_and_qa_fix() {
    let h = Harness::new().await;
    let task = h
        .task(
            "Add user profile page",
            "Create a profile page showing the user's name and avatar, with an edit form, \
             validation, persistence through the existing API client, and tests for the \
             form behaviour and the API integration. It should follow the design system.",
        )
        .await;
    h.router.route(
        ASSESSOR,
        None,
        vec![json(
            json!({"complexity": "standard", "confidence": 0.9, "reasoning": "several files",
                         "needs_research": false, "needs_critique": false, "risk_level": "medium"}),
        )],
    );
    h.router.route(GATHERER, None, vec![spec_json("gathered")]);
    h.router
        .route(WRITER, None, vec![spec_json("Profile page spec")]);
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Foundations", "parallel": true, "subtasks": [
                {"title": "Model", "description": "types"},
                {"title": "Client", "description": "api"}
            ]},
            {"name": "UI", "subtasks": [
                {"title": "Page", "description": "page", "depends_on": ["Model", "Client"]}
            ]}
        ]))],
    );
    let delay = Duration::from_millis(80);
    h.router.route_delayed(
        CODER,
        Some(&subtask_key(1, 3, "Model")),
        delay,
        vec![coder_done("model")],
    );
    h.router.route_delayed(
        CODER,
        Some(&subtask_key(2, 3, "Client")),
        delay,
        vec![coder_done("client")],
    );
    h.router.route(
        CODER,
        Some(&subtask_key(3, 3, "Page")),
        vec![coder_done("page")],
    );
    h.router.route(
        REVIEWER,
        None,
        vec![
            qa("changes_requested", &["Missing test"]),
            qa("approved", &[]),
        ],
    );
    h.router.route(FIXER, None, vec![fixer_done()]);

    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();

    assert_eq!(report.final_status, TaskStatus::Ready, "{report:#?}");
    assert_eq!(
        phases(&report),
        vec![
            Phase::Assess,
            Phase::Spec,
            Phase::Plan,
            Phase::Build,
            Phase::Qa,
            Phase::Fix,
            Phase::Qa,
            Phase::Merge
        ]
    );
    assert_eq!(report.task.complexity, Some(Complexity::Standard));
    // Model and Client ran concurrently; Page only after both.
    assert!(
        h.router.max_in_flight() >= 2,
        "subtasks did not run in parallel"
    );
    let coder_keys: Vec<String> = h
        .router
        .calls_for("coder")
        .into_iter()
        .filter_map(|c| c.key)
        .collect();
    assert_eq!(coder_keys.len(), 3);
    assert_eq!(coder_keys[2], subtask_key(3, 3, "Page"));
    let page = &h.router.calls_for("coder")[2];
    assert!(
        page.system
            .contains("Builds on (already done): Model, Client")
    );

    // Spec from the writer, saved with its markdown rendering.
    let spec = h.store.load_spec(task.id).await.unwrap().unwrap();
    assert_eq!(spec.summary, "Profile page spec");
    let dir = h.task_dir(&task).await;
    assert!(dir.join("spec.md").exists());
    // The writer saw the gatherer output as prior context.
    assert!(
        h.router.calls_for("writer")[0]
            .system
            .contains("\"gathered\"")
    );
    // Findings went to the patterns memory, which later agents receive.
    let memory = h.store.load_memory(task.id).await.unwrap();
    assert!(memory.contains("tests live in tests/"));
    assert!(
        h.router.calls_for("planner")[0]
            .system
            .contains("tests live in tests/")
    );

    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    assert!(plan.subtasks().all(|s| s.status == SubtaskStatus::Done));
    let reports = h.store.load_qa_reports(task.id).await.unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].issues[0].title, "Missing test");
    // The fixer got the report; the second review saw the first round.
    assert!(
        h.router.calls_for("fixer")[0]
            .system
            .contains("Missing test")
    );
    assert!(h.router.calls_for("reviewer")[1].system.contains("Round 1"));
    // Parallel subtasks are committed together, once neither is running.
    let commits = h.commits.lock().unwrap().clone();
    assert_eq!(commits.len(), 3, "{commits:?}");
    assert!(commits[0].starts_with("vibe: complete subtasks 1, 2 - "));
    assert!(commits[0].contains("Model") && commits[0].contains("Client"));
    assert_eq!(commits[1], "vibe: complete subtask 3 - Page");
    assert_eq!(commits[2], "vibe: address QA round 1");

    let events = h.collector.events();
    let done_updates = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SubtaskUpdated {
                    status: SubtaskStatus::Done,
                    ..
                }
            )
        })
        .count();
    assert_eq!(done_updates, 3);
    let statuses: Vec<Phase> = events
        .iter()
        .filter_map(|e| match e {
            Event::PhaseStarted { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(statuses.len(), 8);
}

#[tokio::test]
async fn failing_subtask_is_marked_failed_and_pipeline_continues() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_parallel_subtasks = 1;
    let task = h
        .task("Add caching layer", "Add a cache in front of the store")
        .await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Work", "parallel": true, "subtasks": [
                {"title": "Good", "description": "g"},
                {"title": "Bad", "description": "b"},
                {"title": "Needs bad", "description": "n", "depends_on": ["Bad"]}
            ]}
        ]))],
    );
    h.router.route(
        CODER,
        Some(&subtask_key(1, 3, "Good")),
        vec![coder_done("ok")],
    );
    h.router.route(
        CODER,
        Some(&subtask_key(2, 3, "Bad")),
        vec![coder_failed("compile error"), coder_failed("still broken")],
    );
    h.router.route(
        RECOVERY,
        Some(&subtask_key(2, 3, "Bad")),
        vec![coder_failed("impossible")],
    );
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);

    let report = h
        .pipeline()
        .run(
            task.id,
            RunOptions {
                complexity_override: Some(Complexity::Trivial),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(report.final_status, TaskStatus::Ready, "{report:#?}");
    let build = report
        .phases
        .iter()
        .find(|p| p.phase == Phase::Build)
        .unwrap();
    assert!(!build.success);
    assert!(build.summary.contains("1 done, 1 failed, 1 skipped"));
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    let by_title = |t: &str| plan.subtasks().find(|s| s.title == t).unwrap().clone();
    assert_eq!(by_title("Good").status, SubtaskStatus::Done);
    let bad = by_title("Bad");
    assert_eq!(bad.status, SubtaskStatus::Failed);
    assert_eq!(bad.attempts, 3);
    assert!(bad.notes.contains("Attempt 1 failed") && bad.notes.contains("Attempt 3 failed"));
    assert_eq!(by_title("Needs bad").status, SubtaskStatus::Skipped);
    // The last attempt used the recovery agent with the failure history.
    let recovery = h.router.calls_for("recovery");
    assert_eq!(recovery.len(), 1);
    assert!(recovery[0].system.contains("Attempt 2 failed"));
    assert_eq!(
        h.router.calls_for("assessor").len(),
        0,
        "override skips the assessor"
    );
    let gotchas = h.store.load_memory(task.id).await.unwrap();
    assert!(gotchas.contains("Subtask `Bad` failed repeatedly"));
    let retries = h
        .collector
        .events()
        .iter()
        .filter(|e| matches!(e, Event::Retrying { .. }))
        .count();
    assert_eq!(retries, 2);
}

#[tokio::test]
async fn build_fails_when_no_subtask_succeeds() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_subtask_attempts = 1;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Only", "description": "o"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_failed("nope")]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Failed);
    assert_eq!(report.phases.last().unwrap().phase, Phase::Build);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Failed);
    assert!(state.last_error.unwrap().contains("build failed"));
    assert!(h.router.calls_for("reviewer").is_empty());

    // Resuming the failed run retries the failed subtask with a fresh budget.
    h.router
        .route(CODER, None, vec![coder_done("now it works")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let resumed = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Ready, "{resumed:#?}");
    assert_eq!(
        phases(&resumed),
        vec![Phase::Build, Phase::Qa, Phase::Merge]
    );
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    let only = plan.subtasks().next().unwrap();
    assert_eq!(only.status, SubtaskStatus::Done);
    assert_eq!(only.attempts, 1);
}

#[tokio::test]
async fn qa_never_approving_stops_after_max_rounds() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_qa_rounds = 2;
    let task = h.task("Rename helper", "rename foo to bar").await;
    assert_eq!(
        vibe_pipeline::heuristic_complexity(&task),
        Some(Complexity::Simple)
    );
    h.router.route(GATHERER, None, vec![spec_json("rename")]);
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Rename", "description": "r"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("renamed")]);
    h.router.route(
        REVIEWER,
        None,
        vec![
            qa("changes_requested", &["A"]),
            qa("changes_requested", &["B"]),
        ],
    );
    h.router
        .route(FIXER, None, vec![fixer_done(), fixer_done()]);

    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();

    assert_eq!(report.final_status, TaskStatus::Review);
    assert_eq!(
        phases(&report),
        vec![
            Phase::Assess,
            Phase::Spec,
            Phase::Plan,
            Phase::Build,
            Phase::Qa,
            Phase::Fix,
            Phase::Qa
        ]
    );
    assert!(
        h.router.calls_for("writer").is_empty(),
        "simple profile: light spec"
    );
    assert_eq!(h.router.calls_for("fixer").len(), 1);
    assert_eq!(h.store.load_qa_reports(task.id).await.unwrap().len(), 2);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert!(h.collector.events().iter().any(
        |e| matches!(e, Event::Paused { reason, .. } if reason.contains("did not approve after 2"))
    ));
}

#[tokio::test]
async fn same_issue_three_rounds_in_a_row_escalates() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_qa_rounds = 6;
    let task = h.task("Rename helper", "rename foo to bar").await;
    h.router.route(GATHERER, None, vec![spec_json("rename")]);
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Rename", "description": "r"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("renamed")]);
    h.router.route(
        REVIEWER,
        None,
        vec![
            qa("changes_requested", &["Tests fail"]),
            qa("changes_requested", &["Tests fail", "Style"]),
            qa("changes_requested", &["tests fail"]),
        ],
    );
    h.router
        .route(FIXER, None, vec![fixer_done(), fixer_done()]);

    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();

    assert_eq!(report.final_status, TaskStatus::Review);
    assert_eq!(h.router.calls_for("fixer").len(), 2);
    assert_eq!(report.phases.last().unwrap().phase, Phase::Fix);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.current_phase, Phase::Qa, "resume re-reviews first");
    assert!(
        h.collector
            .events()
            .iter()
            .any(|e| matches!(e, Event::Paused { reason, .. } if reason.contains("tests fail")))
    );
}

#[tokio::test]
async fn interrupted_run_resumes_without_redoing_work() {
    let h = Harness::new().await;
    let task = h.task("Add export command", "Export tasks as CSV").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Seq", "parallel": false, "subtasks": [
                {"title": "First", "description": "a"},
                {"title": "Second", "description": "b"}
            ]}
        ]))],
    );
    let first = subtask_key(1, 2, "First");
    let second = subtask_key(2, 2, "Second");
    h.router
        .route(CODER, Some(&first), vec![coder_done("first")]);
    h.router
        .route(CODER, Some(&second), vec![coder_done("second")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);

    // Cancel as soon as the first subtask is committed.
    let (tx, rx) = watch::channel(false);
    *h.cancel_on_commit.lock().unwrap() = Some(tx);
    let pipeline = h.pipeline();
    let interrupted = pipeline
        .run(
            task.id,
            RunOptions {
                complexity_override: Some(Complexity::Trivial),
                cancel: Some(rx),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(interrupted.final_status, TaskStatus::Cancelled);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Cancelled);
    assert_eq!(state.current_phase, Phase::Build);
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    let statuses: Vec<SubtaskStatus> = plan.subtasks().map(|s| s.status).collect();
    assert_eq!(statuses, vec![SubtaskStatus::Done, SubtaskStatus::Pending]);

    let resumed = pipeline.resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Ready, "{resumed:#?}");
    assert_eq!(resumed.run_id, interrupted.run_id);
    assert_eq!(
        phases(&resumed),
        vec![Phase::Build, Phase::Qa, Phase::Merge]
    );
    assert_eq!(
        h.router.calls_with_key(&first),
        1,
        "done subtask was redone"
    );
    assert_eq!(h.router.calls_with_key(&second), 1);
    assert_eq!(h.router.calls_for("planner").len(), 1);
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    assert!(plan.subtasks().all(|s| s.status == SubtaskStatus::Done));
    // A finished run cannot be resumed.
    assert!(pipeline.resume(task.id).await.is_err());
}

#[tokio::test]
async fn dry_run_stops_after_plan_and_resume_continues() {
    let h = Harness::new().await;
    let task = h
        .task(
            "Improve search",
            "Make the search faster and add fuzzy matching across all indexed fields, \
             with ranking by recency and relevance, configurable weights and tests.",
        )
        .await;
    // No assessor route: the assessment fails and falls back to standard.
    h.router.route(GATHERER, None, vec![spec_json("search")]);
    h.router.route(WRITER, None, vec![spec_json("Search spec")]);
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Index", "description": "i"}]}
        ]))],
    );
    let pipeline = h.pipeline();
    let report = pipeline
        .run(
            task.id,
            RunOptions {
                dry_run: true,
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        phases(&report),
        vec![Phase::Assess, Phase::Spec, Phase::Plan]
    );
    assert_eq!(report.final_status, TaskStatus::Backlog);
    assert_eq!(report.task.complexity, Some(Complexity::Standard));
    assert!(report.phases[0].summary.contains("fallback"));
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.current_phase, Phase::Build);
    assert!(h.router.calls_for("coder").is_empty());

    h.router.route(CODER, None, vec![coder_done("indexed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let resumed = pipeline.resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Ready);
    assert_eq!(
        phases(&resumed),
        vec![Phase::Build, Phase::Qa, Phase::Merge]
    );
    assert_eq!(h.router.calls_for("planner").len(), 1);
}

#[tokio::test]
async fn hook_abort_cancels_the_run() {
    let mut h = Harness::new().await;
    h.registry.add_hook(Arc::new(AbortPhase(Phase::Plan)));
    let task = h.task("Fix typo in docs", "").await;
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Cancelled);
    assert_eq!(phases(&report), vec![Phase::Assess]);
    assert!(h.router.calls_for("planner").is_empty());
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Cancelled);
    assert_eq!(state.current_phase, Phase::Plan);
    assert!(state.last_error.unwrap().contains("not today"));
    assert!(h.collector.events().iter().any(|e| matches!(
        e,
        Event::RunFinished {
            success: false,
            status: TaskStatus::Cancelled,
            ..
        }
    )));
    assert_eq!(
        h.store.load_task(task.id).await.unwrap().status,
        TaskStatus::Cancelled
    );
}

#[tokio::test]
async fn invalid_plan_is_retried_with_feedback() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![
            plan_json(json!([{"name": "W", "subtasks": [
                {"title": "A", "description": "a", "depends_on": ["Ghost"]}
            ]}])),
            plan_json(json!([{"name": "W", "subtasks": [{"title": "A", "description": "a"}]}])),
        ],
    );
    h.router.route(CODER, None, vec![coder_done("a")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    let planner = h.router.calls_for("planner");
    assert_eq!(planner.len(), 2);
    assert!(planner[1].system.contains("unknown subtask `Ghost`"));
}

#[tokio::test]
async fn planner_that_never_succeeds_fails_the_run() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_phase_retries = 1;
    let task = h.task("Fix typo in docs", "").await;
    let empty = || plan_json(json!([]));
    h.router.route(PLANNER, None, vec![empty(), empty()]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Failed);
    assert_eq!(h.router.calls_for("planner").len(), 2);
    let last = report.phases.last().unwrap();
    assert_eq!(last.phase, Phase::Plan);
    assert!(!last.success);
    let progress = h.store.load_progress(task.id).await.unwrap();
    assert!(progress.contains("Phase plan failed"));
}

#[tokio::test]
async fn complex_profile_runs_research_and_critique() {
    let h = Harness::new().await;
    let task = h
        .task("Migrate storage", "Move persistence to SQLite")
        .await;
    h.router.route(GATHERER, None, vec![spec_json("gathered")]);
    h.router.route(
        RESEARCHER,
        None,
        vec![vibe_agents::test_support::text_response(
            "**Dependencies**: rusqlite 0.31",
        )],
    );
    h.router.route(WRITER, None, vec![spec_json("written")]);
    h.router.route(
        CRITIC,
        None,
        vec![json(
            json!({"verdict": "revised", "issues": [{"title": "vague"}],
                          "spec": {"summary": "criticised", "requirements": []}}),
        )],
    );
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Do", "description": "d"}]}
        ]))],
    );
    let report = h
        .pipeline()
        .run(
            task.id,
            RunOptions {
                complexity_override: Some(Complexity::Complex),
                until_phase: Some(Phase::Plan),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        phases(&report),
        vec![Phase::Assess, Phase::Spec, Phase::Plan]
    );
    assert!(
        h.router.calls_for("writer")[0]
            .system
            .contains("rusqlite 0.31")
    );
    assert!(h.router.calls_for("critic")[0].system.contains("written"));
    let spec = h.store.load_spec(task.id).await.unwrap().unwrap();
    assert_eq!(spec.summary, "criticised");
    assert!(
        report.phases[1]
            .summary
            .contains("critic: revised (1 issues)")
    );
}

#[tokio::test]
async fn auto_merge_conflicts_leave_task_ready() {
    use vibe_core::{MergeOutcome, Workspace, WorkspaceProvider};
    struct Conflicting;
    #[async_trait::async_trait]
    impl WorkspaceProvider for Conflicting {
        fn name(&self) -> &str {
            "conflicting"
        }
        async fn open(
            &self,
            root: &std::path::Path,
            t: &vibe_core::Task,
        ) -> vibe_core::Result<Workspace> {
            vibe_core::InPlaceWorkspace.open(root, t).await
        }
        async fn merge(&self, _w: &Workspace) -> vibe_core::Result<MergeOutcome> {
            Ok(MergeOutcome::NeedsHumanReview {
                files: vec!["src/a.rs".into()],
            })
        }
        async fn discard(&self, _w: &Workspace) -> vibe_core::Result<()> {
            Ok(())
        }
        async fn changes(&self, _w: &Workspace) -> vibe_core::Result<String> {
            Ok("DIFF-MARKER".into())
        }
    }
    let mut h = Harness::new().await;
    h.config.pipeline.auto_merge = true;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "A", "description": "a"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("a")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let store: Arc<dyn PipelineStore> = h.store.clone();
    let pipeline = vibe_pipeline::Pipeline::new(vibe_pipeline::PipelineDeps {
        registry: Arc::new(h.registry.clone()),
        providers: Arc::new(Resolver(h.router.clone())),
        store,
        workspace: Arc::new(Conflicting),
        tools: vibe_core::ToolRegistry::new(),
        events: h.events.clone(),
        config: h.config.clone(),
        project_root: h.root(),
        committer: None,
        resetter: None,
        subtask_workspaces: None,
    });
    let report = pipeline.run(task.id, RunOptions::default()).await.unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    assert!(!report.phases.last().unwrap().success);
    assert!(
        h.router.calls_for("reviewer")[0]
            .user
            .contains("DIFF-MARKER")
    );
    let progress = h.store.load_progress(task.id).await.unwrap();
    assert!(progress.contains("`src/a.rs`"));
    assert!(progress.contains("`manual` conflict strategy"));
    // Conflicts merge nothing: no `merged` event.
    assert!(
        !h.collector
            .events()
            .iter()
            .any(|e| matches!(e, Event::Merged { .. }))
    );
    // Memory append path is exercised elsewhere; the store API stays usable.
    h.store
        .append_memory(task.id, MemoryFile::Gotchas, "x")
        .await
        .unwrap();
}

#[tokio::test]
async fn dropped_run_aborts_coder_sessions() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Slow", "description": "s"}]}
        ]))],
    );
    h.router.route_delayed(
        CODER,
        None,
        Duration::from_millis(300),
        vec![coder_done("too late")],
    );
    let pipeline = h.pipeline();
    let timed_out = tokio::time::timeout(
        Duration::from_millis(100),
        pipeline.run(task.id, RunOptions::default()),
    )
    .await;
    assert!(timed_out.is_err(), "the run should still be building");
    assert_eq!(h.router.calls_for("coder").len(), 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        h.router.completed_for("coder"),
        0,
        "a coder session outlived the dropped run"
    );
}

/// The CLI may `tokio::spawn` a run: its futures must be `Send`.
#[allow(dead_code)]
fn run_futures_are_send(pipeline: &vibe_pipeline::Pipeline, id: vibe_core::TaskId) {
    fn assert_send<T: Send>(_: &T) {}
    assert_send(&pipeline.run(id, RunOptions::default()));
    assert_send(&pipeline.resume(id));
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<vibe_pipeline::Pipeline>();
}

#[tokio::test]
async fn failed_attempt_is_reset_before_the_retry() {
    let mut h = Harness::new().await;
    h.use_resetter = true;
    h.config.pipeline.max_parallel_subtasks = 1;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Flaky", "description": "f"}]}
        ]))],
    );
    h.router.route(
        CODER,
        None,
        vec![coder_failed("broke the build"), coder_done("fixed")],
    );
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    assert_eq!(h.resets(), 1);
    assert_eq!(
        h.journal.lock().unwrap().clone(),
        vec![
            "reset".to_string(),
            "commit: vibe: complete subtask 1 - Flaky".to_string()
        ]
    );
    let progress = h.store.load_progress(task.id).await.unwrap();
    assert!(progress.contains("Workspace reset after attempt 1 of subtask 1 `Flaky`"));
}

#[tokio::test]
async fn reset_waits_for_running_sessions_and_blocks_new_ones() {
    let mut h = Harness::new().await;
    h.use_resetter = true;
    h.config.pipeline.max_parallel_subtasks = 2;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "parallel": true, "subtasks": [
                {"title": "Slow", "description": "s"},
                {"title": "Bad", "description": "b"}
            ]}
        ]))],
    );
    h.router.route_delayed(
        CODER,
        Some(&subtask_key(1, 2, "Slow")),
        Duration::from_millis(150),
        vec![coder_done("slow")],
    );
    h.router.route(
        CODER,
        Some(&subtask_key(2, 2, "Bad")),
        vec![coder_failed("oops"), coder_done("bad fixed")],
    );
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    // Bad failed while Slow was running: the reset waited for Slow (whose
    // work was committed first) and Bad's retry only started afterwards.
    assert_eq!(
        h.journal.lock().unwrap().clone(),
        vec![
            "commit: vibe: complete subtask 1 - Slow".to_string(),
            "reset".to_string(),
            "commit: vibe: complete subtask 2 - Bad".to_string(),
        ]
    );
}

#[tokio::test]
async fn without_resetter_leftovers_are_noted() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_subtask_attempts = 2;
    let task = h.task("Fix typo in docs", "").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "W", "subtasks": [{"title": "Flaky", "description": "f"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_failed("nope")]);
    h.router
        .route(RECOVERY, None, vec![coder_done("recovered")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    assert_eq!(h.resets(), 0);
    let progress = h.store.load_progress(task.id).await.unwrap();
    assert!(progress.contains("attempt 1 of subtask 1 `Flaky` left uncommitted changes in place"));
}

#[tokio::test]
async fn required_validation_fails_closed_without_shell_tool() {
    let mut h = Harness::new().await;
    h.config.pipeline.validation_commands = vec!["exit 0".into()];
    h.config.pipeline.auto_merge = true;
    let task = h.task("Fix typo", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Only", "subtasks": [{"title": "Fix typo", "description": "edit"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Review);
    assert!(!report.is_success());
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert!(!state.validations[0].passed);
    assert!(state.validations[0].output.contains("registered bash tool"));
}

/// Scripted shell outcomes let these tests observe the fixer context and replay order.
struct ValidationShell {
    outcomes: std::sync::Mutex<std::collections::VecDeque<bool>>,
    commands: std::sync::Mutex<Vec<String>>,
}

impl ValidationShell {
    fn new(outcomes: &[bool]) -> Arc<Self> {
        Arc::new(Self {
            outcomes: std::sync::Mutex::new(outcomes.iter().copied().collect()),
            commands: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl vibe_core::Tool for ValidationShell {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Scripted validation shell"
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({})
    }
    async fn call(
        &self,
        ctx: &vibe_core::ToolContext,
        input: serde_json::Value,
    ) -> vibe_core::Result<vibe_core::ToolOutput> {
        assert_eq!(ctx.agent, "pipeline_validation");
        assert!(ctx.workspace_root.exists());
        self.commands
            .lock()
            .unwrap()
            .push(input["command"].as_str().unwrap().into());
        let passed = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected validation call");
        Ok(vibe_core::ToolOutput {
            content: if passed {
                "tests passed"
            } else {
                "assertion failed: expected upper bound to be inclusive"
            }
            .into(),
            is_error: !passed,
            metadata: json!({"exit_code": if passed { 0 } else { 1 }, "timed_out": false}),
        })
    }
}

async fn validation_harness(
    outcomes: &[bool],
    fixes: usize,
) -> (Harness, vibe_core::Task, Arc<ValidationShell>) {
    let mut h = Harness::new().await;
    h.config.pipeline.validation_commands = vec!["cargo test".into()];
    let shell = ValidationShell::new(outcomes);
    h.tools.register(shell.clone());
    let task = h.task("Fix typo", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Only", "subtasks": [{"title": "Fix typo", "description": "edit"}]}
        ]))],
    );
    h.router.route(CODER, None, vec![coder_done("fixed")]);
    h.router.route(
        REVIEWER,
        None,
        (0..=fixes).map(|_| qa("approved", &[])).collect(),
    );
    h.router
        .route(FIXER, None, (0..fixes).map(|_| fixer_done()).collect());
    (h, task, shell)
}

#[tokio::test]
async fn validation_failure_reaches_fixer_and_all_checks_replay_after_qa() {
    let (mut h, task, shell) = validation_harness(&[true, false, true, true], 1).await;
    h.config.pipeline.validation_commands = vec!["cargo check".into(), "cargo test".into()];
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    assert_eq!(report.state.validation_fix_attempts, 1);
    assert_eq!(report.state.pending_validation_fix, None);
    assert_eq!(
        *shell.commands.lock().unwrap(),
        vec!["cargo check", "cargo test", "cargo check", "cargo test"]
    );
    assert_eq!(
        phases(&report),
        vec![
            Phase::Assess,
            Phase::Plan,
            Phase::Build,
            Phase::Qa,
            Phase::Merge,
            Phase::Fix,
            Phase::Qa,
            Phase::Merge
        ]
    );
    let fixer = &h.router.calls_for("fixer")[0];
    assert!(fixer.system.contains("cargo test"));
    assert!(
        fixer
            .system
            .contains("expected upper bound to be inclusive")
    );
    assert!(
        fixer
            .system
            .contains("Do not change the validation command")
    );
    assert_eq!(h.router.calls_for("reviewer").len(), 2);
    assert!(
        h.commits
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("required validation attempt 1"))
    );
}

#[tokio::test]
async fn validation_fix_budget_survives_resume_and_manual_repair_still_passes() {
    let (h, task, _) = validation_harness(&[false, false, false, false, true], 2).await;
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Review);
    assert_eq!(report.state.validation_fix_attempts, 2);
    assert_eq!(h.router.calls_for("fixer").len(), 2);
    let resumed = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Review);
    assert_eq!(resumed.state.validation_fix_attempts, 2);
    assert_eq!(
        h.router.calls_for("fixer").len(),
        2,
        "resume must not reset the budget"
    );
    let repaired = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(repaired.final_status, TaskStatus::Ready);
    assert_eq!(repaired.state.validations.len(), 5);
    assert_eq!(repaired.state.validation_fix_attempts, 2);
}

#[tokio::test]
async fn interrupted_validation_fix_with_consumed_budget_does_not_call_model() {
    let (h, task, _) = validation_harness(&[false, false, false], 2).await;
    h.pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    // Simulate a crash after reserving the last attempt but before advancing to QA.
    let mut state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    state.current_phase = Phase::Fix;
    state.pending_validation_fix = Some(2);
    state.status = RunStatus::Running;
    h.store.save_run_state(&state).await.unwrap();
    let report = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(report.final_status, TaskStatus::Review);
    assert_eq!(h.router.calls_for("fixer").len(), 2);
    assert_eq!(report.state.validation_fix_attempts, 2);
    assert_eq!(report.state.pending_validation_fix, None);
}

#[tokio::test]
async fn validation_fix_resumes_after_a_hook_interrupts_the_fix_phase() {
    let (mut h, task, _) = validation_harness(&[false, true], 1).await;
    h.registry.add_hook(Arc::new(AbortPhase(Phase::Fix)));
    let interrupted = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(interrupted.final_status, TaskStatus::Cancelled);
    assert_eq!(interrupted.state.current_phase, Phase::Fix);
    assert_eq!(interrupted.state.pending_validation_fix, Some(0));
    assert_eq!(interrupted.state.validation_fix_attempts, 0);
    assert!(h.router.calls_for("fixer").is_empty());
    h.registry = vibe_core::Registry::new();
    let resumed = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Ready);
    assert_eq!(resumed.state.validation_fix_attempts, 1);
    assert_eq!(resumed.state.pending_validation_fix, None);
    assert_eq!(h.router.calls_for("fixer").len(), 1);
}

#[tokio::test]
async fn token_budget_pauses_and_survives_resume() {
    let mut h = Harness::new().await;
    // Every scripted answer costs 15 tokens: planner + first coder = 30.
    h.config.pipeline.max_tokens = Some(30);
    let task = h.task("Add export command", "Export tasks as CSV").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Seq", "parallel": false, "subtasks": [
                {"title": "First", "description": "a"},
                {"title": "Second", "description": "b"}
            ]}
        ]))],
    );
    let first = subtask_key(1, 2, "First");
    let second = subtask_key(2, 2, "Second");
    h.router
        .route(CODER, Some(&first), vec![coder_done("first")]);
    h.router
        .route(CODER, Some(&second), vec![coder_done("second")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let options = || RunOptions {
        complexity_override: Some(Complexity::Trivial),
        ..RunOptions::default()
    };

    let paused = h.pipeline().run(task.id, options()).await.unwrap();
    assert_eq!(paused.final_status, TaskStatus::Backlog);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.current_phase, Phase::Build);
    assert_eq!(state.usage.total(), 30);
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("token budget exhausted (30 of 30"))
    );
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    let statuses: Vec<SubtaskStatus> = plan.subtasks().map(|s| s.status).collect();
    assert_eq!(statuses, vec![SubtaskStatus::Done, SubtaskStatus::Pending]);
    assert!(h.collector.events().iter().any(|e| matches!(
        e,
        Event::Paused { reason, .. } if reason.contains("raise the limit")
    )));

    // Resuming without raising the limit spends nothing.
    let again = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(again.final_status, TaskStatus::Backlog);
    assert!(again.phases.is_empty());
    assert_eq!(h.router.calls_with_key(&second), 0);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.usage.total(), 30);

    h.config.pipeline.max_tokens = Some(1_000);
    let resumed = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(resumed.final_status, TaskStatus::Ready, "{resumed:#?}");
    assert_eq!(resumed.run_id, paused.run_id);
    assert_eq!(resumed.usage.total(), 30, "this invocation only");
    assert_eq!(h.router.calls_with_key(&first), 1);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Finished);
    assert_eq!(state.usage.total(), 60, "whole run");
}

#[tokio::test]
async fn duration_budget_counts_earlier_invocations() {
    let mut h = Harness::new().await;
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([{"name": "Only", "subtasks": [
            {"title": "Fix the typo", "description": "edit README"}
        ]}]))],
    );
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let paused = h
        .pipeline()
        .run(
            task.id,
            RunOptions {
                until_phase: Some(Phase::Plan),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(paused.final_status, TaskStatus::Backlog);

    // Pretend the first invocation took an hour: a 30 minute budget is spent.
    let mut state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert!(state.usage.total() > 0);
    state.active_ms = 3_600_000;
    h.store.save_run_state(&state).await.unwrap();
    h.config.pipeline.max_duration_secs = Some(1_800);
    let stopped = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(stopped.final_status, TaskStatus::Backlog);
    assert!(h.router.calls_for("coder").is_empty());
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert!(state.active_ms >= 3_600_000);
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("duration budget exhausted"))
    );

    h.config.pipeline.max_duration_secs = None;
    let done = h.pipeline().resume(task.id).await.unwrap();
    assert_eq!(done.final_status, TaskStatus::Ready);
}

#[tokio::test]
async fn isolated_subtasks_integrate_one_at_a_time_and_retry_conflicts() {
    let mut h = Harness::new().await;
    let fake = Arc::new(FakeSubtasks::default());
    fake.conflict_once.lock().unwrap().push("s2-".into());
    h.subtasks = Some(fake.clone());
    let task = h.task("Add export command", "Export tasks as CSV").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([
            {"name": "Par", "parallel": true, "subtasks": [
                {"title": "First", "description": "a"},
                {"title": "Second", "description": "b"}
            ]}
        ]))],
    );
    let first = subtask_key(1, 2, "First");
    let second = subtask_key(2, 2, "Second");
    // The first subtask is slow, so the second finishes (and conflicts) first.
    h.router.route_delayed(
        CODER,
        Some(&first),
        Duration::from_millis(80),
        vec![coder_done("first")],
    );
    h.router.route(
        CODER,
        Some(&second),
        vec![coder_done("second"), coder_done("second again")],
    );
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);

    let report = h
        .pipeline()
        .run(
            task.id,
            RunOptions {
                complexity_override: Some(Complexity::Trivial),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready, "{report:#?}");
    assert_eq!(h.router.max_in_flight(), 2, "subtasks ran in parallel");
    assert_eq!(h.router.calls_with_key(&second), 2, "conflict was retried");

    let log = fake.log();
    assert_eq!(log.first().map(String::as_str), Some("discard_all"));
    assert_eq!(log.last().map(String::as_str), Some("discard_all"));
    let integrations: Vec<&str> = log
        .iter()
        .filter(|l| l.starts_with("integrate"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        integrations,
        vec!["integrate s2-a1", "integrate s2-a2", "integrate s1-a1"]
    );
    for label in ["s1-a1", "s2-a1", "s2-a2"] {
        assert!(log.contains(&format!("open {label}")), "{log:?}");
        assert!(log.contains(&format!("discard {label}")), "{log:?}");
    }
    // Subtasks are integrated, not committed through the shared committer.
    assert_eq!(
        h.commits.lock().unwrap().clone(),
        vec!["vibe: checkpoint before build".to_string()]
    );
    // Each attempt worked in its own workspace.
    let coders = h.router.calls_for("coder");
    assert!(
        coders
            .iter()
            .any(|c| c.system.contains("attempt-s1-a1") || c.user.contains("attempt-s1-a1"))
    );
    let plan = h.store.load_plan(task.id).await.unwrap().unwrap();
    assert!(plan.subtasks().all(|s| s.status == SubtaskStatus::Done));
    let progress = h.store.load_progress(task.id).await.unwrap();
    assert!(
        progress.contains("conflict with work integrated since the attempt started"),
        "{progress}"
    );
    // The checkpoint and every successful integration are announced; the
    // integrations carry their subtask.
    let committed: Vec<(Option<vibe_core::SubtaskId>, String)> = h
        .collector
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Committed {
                subtask, commit, ..
            } => Some((subtask, commit)),
            _ => None,
        })
        .collect();
    let ids: Vec<vibe_core::SubtaskId> = plan.subtasks().map(|s| s.id).collect();
    assert_eq!(
        committed,
        vec![
            (None, "abc123".to_string()),
            (Some(ids[1]), "sha-s2-a2".to_string()),
            (Some(ids[0]), "sha-s1-a1".to_string()),
        ]
    );
}

#[tokio::test]
async fn isolation_can_be_turned_off() {
    let mut h = Harness::new().await;
    let fake = Arc::new(FakeSubtasks::default());
    h.subtasks = Some(fake.clone());
    h.config.pipeline.isolate_subtasks = false;
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([{"name": "Only", "subtasks": [
            {"title": "Fix the typo", "description": "edit README"}
        ]}]))],
    );
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let report = h
        .pipeline()
        .run(task.id, RunOptions::default())
        .await
        .unwrap();
    assert_eq!(report.final_status, TaskStatus::Ready);
    assert!(fake.log().is_empty());
    assert_eq!(
        h.commits.lock().unwrap().clone(),
        vec!["vibe: complete subtask 1 - Fix the typo".to_string()]
    );
}

#[tokio::test]
async fn logged_events_are_numbered_across_resumes() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(
        PLANNER,
        None,
        vec![plan_json(json!([{"name": "Only", "subtasks": [
            {"title": "Fix the typo", "description": "edit README"}
        ]}]))],
    );
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let pipeline = h.pipeline();
    let first = pipeline
        .run(
            task.id,
            RunOptions {
                until_phase: Some(Phase::Plan),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    pipeline.resume(task.id).await.unwrap();

    let events = h.store.load_events(task.id).await.unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e.seq.unwrap()).collect();
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(
        seqs, expected,
        "one run, numbered without gaps across the resume"
    );
    assert!(
        events
            .iter()
            .all(|e| e.event.run_id() == Some(first.run_id))
    );
    assert!(
        events
            .iter()
            .all(|e| e.schema == vibe_core::EVENT_SCHEMA_VERSION)
    );
    let has = |pred: &dyn Fn(&Event) -> bool| events.iter().any(|e| pred(&e.event));
    assert!(has(&|e| matches!(
        e,
        Event::ArtefactWritten {
            artefact: vibe_core::Artefact::Plan,
            ..
        }
    )));
    assert!(has(&|e| matches!(
        e,
        Event::ArtefactWritten {
            artefact: vibe_core::Artefact::QaReport { round: 1 },
            ..
        }
    )));
    assert!(has(
        &|e| matches!(e, Event::BudgetUpdated { tokens, .. } if *tokens > 0)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.event, Event::RunStarted { .. }))
            .count(),
        2
    );
}

async fn approve(h: &Harness, task: vibe_core::TaskId, approved: bool, comment: &str) {
    let mut state = h.store.load_run_state(task).await.unwrap().unwrap();
    state.resolve_approval(approved, comment).unwrap();
    h.store.save_run_state(&state).await.unwrap();
}

fn one_subtask_plan() -> vibe_core::CompletionResponse {
    plan_json(json!([{"name": "Only", "subtasks": [
        {"title": "Fix the typo", "description": "edit README"}
    ]}]))
}

#[tokio::test]
async fn plan_gate_pauses_until_approved() {
    let mut h = Harness::new().await;
    h.config.pipeline.approvals = vec![vibe_core::ApprovalGate::Plan];
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(PLANNER, None, vec![one_subtask_plan()]);
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let pipeline = h.pipeline();

    let paused = pipeline.run(task.id, RunOptions::default()).await.unwrap();
    assert_eq!(paused.final_status, TaskStatus::Review);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.current_phase, Phase::Build);
    assert_eq!(state.pending_approval, Some(vibe_core::ApprovalGate::Plan));
    assert!(h.router.calls_for("coder").is_empty());
    assert!(h.collector.events().iter().any(|e| matches!(
        e,
        Event::ApprovalRequested {
            gate: vibe_core::ApprovalGate::Plan,
            ..
        }
    )));

    // Resuming without a decision pauses again, without a new request.
    pipeline.resume(task.id).await.unwrap();
    let requests = h
        .collector
        .events()
        .iter()
        .filter(|e| matches!(e, Event::ApprovalRequested { .. }))
        .count();
    assert_eq!(requests, 1);

    approve(&h, task.id, true, "looks right").await;
    let done = pipeline.resume(task.id).await.unwrap();
    assert_eq!(done.final_status, TaskStatus::Ready, "{done:#?}");
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.approvals.len(), 1);
    assert!(state.approvals[0].approved);
    assert_eq!(state.pending_approval, None);
}

#[tokio::test]
async fn rejected_plan_is_redone_with_the_reason() {
    let mut h = Harness::new().await;
    h.config.pipeline.approvals = vec![vibe_core::ApprovalGate::Plan];
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router
        .route(PLANNER, None, vec![one_subtask_plan(), one_subtask_plan()]);
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    let pipeline = h.pipeline();
    pipeline.run(task.id, RunOptions::default()).await.unwrap();

    approve(&h, task.id, false, "Also fix the same typo in docs/").await;
    let again = pipeline.resume(task.id).await.unwrap();
    assert_eq!(
        again.final_status,
        TaskStatus::Review,
        "waits for the new plan"
    );
    let planners = h.router.calls_for("planner");
    assert_eq!(planners.len(), 2);
    let second = format!("{}{}", planners[1].system, planners[1].user);
    assert!(
        second.contains("Also fix the same typo in docs/"),
        "{second}"
    );
    assert!(!format!("{}{}", planners[0].system, planners[0].user).contains("docs/"));
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.rejection, None, "the planner consumed it");
    assert_eq!(state.pending_approval, Some(vibe_core::ApprovalGate::Plan));

    approve(&h, task.id, true, "").await;
    let done = pipeline.resume(task.id).await.unwrap();
    assert_eq!(done.final_status, TaskStatus::Ready);
}

#[tokio::test]
async fn rejected_merge_goes_to_the_fixer_then_asks_again() {
    let mut h = Harness::new().await;
    h.config.pipeline.approvals = vec![vibe_core::ApprovalGate::Merge];
    let task = h.task("Fix typo in README", "teh -> the").await;
    h.router.route(PLANNER, None, vec![one_subtask_plan()]);
    h.router.route(CODER, None, vec![coder_done("typo fixed")]);
    h.router.route(
        REVIEWER,
        None,
        vec![qa("approved", &[]), qa("approved", &[])],
    );
    h.router.route(FIXER, None, vec![fixer_done()]);
    let pipeline = h.pipeline();
    let options = || RunOptions {
        complexity_override: Some(Complexity::Trivial),
        ..RunOptions::default()
    };
    let paused = pipeline.run(task.id, options()).await.unwrap();
    assert_eq!(paused.final_status, TaskStatus::Review);
    let state = h.store.load_run_state(task.id).await.unwrap().unwrap();
    assert_eq!(state.current_phase, Phase::Merge);

    approve(&h, task.id, false, "The README title must stay in English").await;
    let again = pipeline.resume(task.id).await.unwrap();
    assert_eq!(
        phases(&again),
        vec![Phase::Fix, Phase::Qa],
        "the trivial profile still runs the fixer for a human rejection"
    );
    let fixer = &h.router.calls_for("fixer")[0];
    assert!(format!("{}{}", fixer.system, fixer.user).contains("must stay in English"));
    let reports = h.store.load_qa_reports(task.id).await.unwrap();
    assert_eq!(reports.len(), 3, "review, human review, review");
    assert_eq!(reports[1].issues[0].title, "Human review");

    approve(&h, task.id, true, "ok").await;
    let done = pipeline.resume(task.id).await.unwrap();
    assert_eq!(done.final_status, TaskStatus::Ready);
}

#[test]
fn approvals_need_a_pending_gate_and_rejections_a_reason() {
    let mut state = vibe_pipeline::RunState::new(
        vibe_core::RunId::new(),
        vibe_core::TaskId::new(),
        Phase::Build,
    );
    assert!(state.resolve_approval(true, "").is_err());
    state.pending_approval = Some(vibe_core::ApprovalGate::Plan);
    assert!(state.resolve_approval(false, "  ").is_err());
    assert!(state.resolve_approval(false, "why").is_ok());
    assert_eq!(state.rejection.as_ref().unwrap().comment, "why");
}

fn slow_coder_harness_routes(h: &Harness, delay_ms: u64) {
    h.router
        .route(PLANNER, None, vec![one_subtask_plan(), one_subtask_plan()]);
    h.router.route_delayed(
        CODER,
        None,
        Duration::from_millis(delay_ms),
        vec![coder_done("done"), coder_done("done")],
    );
    h.router.route(
        REVIEWER,
        None,
        vec![qa("approved", &[]), qa("approved", &[])],
    );
}

#[tokio::test]
async fn run_manager_runs_tasks_in_parallel_and_cancels_one() {
    let h = Harness::new().await;
    let a = h.task("Fix typo in README", "teh -> the").await;
    let b = h.task("Fix typo in CHANGELOG", "teh -> the").await;
    slow_coder_harness_routes(&h, 300);
    let manager = vibe_pipeline::RunManager::new(h.pipeline());
    let mut events = manager.subscribe();
    let run_a = manager.start(a.id, RunOptions::default()).unwrap();
    let run_b = manager.start(b.id, RunOptions::default()).unwrap();
    assert!(manager.start(a.id, RunOptions::default()).is_err());
    let mut active = manager.active();
    active.sort();
    let mut expected = vec![a.id, b.id];
    expected.sort();
    assert_eq!(active, expected);

    // Cancel `a` once its coder is working.
    loop {
        let e = events.recv().await.unwrap();
        if let Event::AgentStarted { role, .. } = &e.event
            && *role == vibe_core::AgentRole::Coder
        {
            break;
        }
    }
    assert!(manager.cancel(a.id));
    let ra = run_a.wait().await.unwrap();
    let rb = run_b.wait().await.unwrap();
    assert_eq!(ra.final_status, TaskStatus::Cancelled);
    assert_eq!(rb.final_status, TaskStatus::Ready);
    assert!(manager.active().is_empty());
    assert!(!manager.cancel(a.id));
}

#[tokio::test]
async fn one_process_at_a_time_and_cancel_requests_from_another() {
    let h = Harness::new().await;
    let task = h.task("Fix typo in README", "teh -> the").await;
    slow_coder_harness_routes(&h, 1500);
    let first = vibe_pipeline::RunManager::new(h.pipeline());
    let running = first.start(task.id, RunOptions::default()).unwrap();
    // Wait until the run holds its lock.
    for _ in 0..100 {
        if h.store.is_running(task.id).await.unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(h.store.is_running(task.id).await.unwrap());

    // A second store (another process) cannot run the task...
    let other = vibe_pipeline::FileTaskStore::open(h.root()).unwrap();
    let err = other.lock_run(task.id).await.err().unwrap();
    assert!(err.message.contains("already being run"), "{err}");
    // ...but can ask the running one to stop.
    other.request_cancel(task.id).await.unwrap();
    let report = running.wait().await.unwrap();
    assert_eq!(report.final_status, TaskStatus::Cancelled);
    assert!(!h.store.is_running(task.id).await.unwrap());
    assert!(
        other.request_cancel(task.id).await.is_err(),
        "not running any more"
    );
    // The lock is free again.
    assert!(other.lock_run(task.id).await.unwrap().is_some());
}

#[tokio::test]
async fn lessons_of_one_task_are_recalled_in_the_next() {
    let mut h = Harness::new().await;
    h.config.pipeline.max_subtask_attempts = 1;
    let memory = Arc::new(vibe_pipeline::FileMemoryStore::for_project(&h.root()));
    h.registry.add_memory("project", memory.clone());
    let first = h.task("Add export command", "Export tasks as CSV").await;
    h.router.route(
        PLANNER,
        None,
        vec![
            plan_json(json!([{"name": "Only", "subtasks": [
                {"title": "Write the exporter", "description": "csv"}
            ]}])),
            one_subtask_plan(),
        ],
    );
    h.router.route(
        CODER,
        None,
        vec![coder_failed("the csv crate is not vendored offline")],
    );
    let failed = h
        .pipeline()
        .run(
            first.id,
            RunOptions {
                complexity_override: Some(Complexity::Trivial),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(failed.final_status, TaskStatus::Failed);
    let lessons = memory.all().await.unwrap();
    assert_eq!(lessons.len(), 1);
    assert!(lessons[0].content.contains("not vendored offline"));
    assert_eq!(lessons[0].task_id, Some(first.id));

    let second = h.task("Add export to JSON", "Export tasks as JSON").await;
    h.router.route(CODER, None, vec![coder_done("done")]);
    h.router.route(REVIEWER, None, vec![qa("approved", &[])]);
    h.pipeline()
        .run(
            second.id,
            RunOptions {
                complexity_override: Some(Complexity::Trivial),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
    let planner = &h.router.calls_for("planner")[1];
    let prompt = format!("{}{}", planner.system, planner.user);
    assert!(prompt.contains("Recalled from `project`"), "{prompt}");
    assert!(prompt.contains("not vendored offline"));
}
