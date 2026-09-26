//! Integration tests of the agent loop, structured output and continuation,
//! driven by the scripted provider of `vibe_agents::test_support`.

use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;
use vibe_agents::test_support::{
    BigOutputTool, ConcurrencyProbe, EchoTool, FailingTool, PromptHook, ScriptedProvider,
    SlowMutatingTool, VetoHook, response_with_stop, text_response, tool_use_response,
};
use vibe_agents::{
    AgentRunner, CONTEXT_WARNING_MESSAGE, CONTINUE_NUDGE, CONVERGE_MESSAGE, ContinuationPolicy,
    run_structured, run_with_continuation,
};
use vibe_core::agent::ThinkingLevel;
use vibe_core::{
    AgentRole, AgentSpec, AgentStop, CompletionRequest, ContentBlock, Error, ErrorKind, Event,
    EventBus, Message, Registry, Role, StopReason, Task, Tool, ToolContext, ToolOutput,
    ToolRegistry, ToolSelection,
};

fn spec() -> AgentSpec {
    AgentSpec::new(AgentRole::Custom("tester".into()), "You are a test agent.")
}

fn runner(provider: Arc<ScriptedProvider>, tools: ToolRegistry) -> AgentRunner {
    runner_with_registry(provider, tools, Registry::new())
}

fn runner_with_registry(
    provider: Arc<ScriptedProvider>,
    tools: ToolRegistry,
    registry: Registry,
) -> AgentRunner {
    AgentRunner::new(
        provider,
        "test-model".into(),
        tools,
        Arc::new(registry),
        EventBus::default(),
    )
    .retry_base_delay(Duration::ZERO)
}

/// Tool results carried by the last message of a request.
fn last_tool_results(req: &CompletionRequest) -> Vec<(String, String, bool)> {
    req.messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

fn last_user_text(req: &CompletionRequest) -> String {
    let last = req.messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    last.text()
}

// ---------------------------------------------------------------- scenarios

#[tokio::test]
async fn text_only_run() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("Hello there")]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let out = r.run(&spec(), "Say hello".into()).await.unwrap();

    assert_eq!(out.stop, AgentStop::Completed);
    assert!(out.is_success());
    assert_eq!(out.final_text, "Hello there");
    assert_eq!(out.steps, 1);
    assert_eq!(out.tool_calls, 0);
    assert_eq!(out.usage.input_tokens, 10);
    assert_eq!(out.messages.len(), 2);
    assert_eq!(out.role, AgentRole::Custom("tester".into()));

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].model, "test-model");
    assert_eq!(reqs[0].system, "You are a test agent.");
    assert_eq!(reqs[0].messages, vec![Message::user("Say hello")]);
}

#[tokio::test]
async fn two_tool_calls_in_one_step_run_in_parallel() {
    let probe = ConcurrencyProbe::new();
    let tools = ToolRegistry::new().with(Arc::new(
        EchoTool::new()
            .with_delay(Duration::from_millis(30))
            .with_probe(probe.clone()),
    ));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![
            ("c1", "echo", json!({"text": "first"})),
            ("c2", "echo", json!({"text": "second"})),
        ]),
        text_response("done"),
    ]));
    let out = runner(provider.clone(), tools)
        .run(&spec(), "go".into())
        .await
        .unwrap();

    assert_eq!(out.stop, AgentStop::Completed);
    assert_eq!(out.tool_calls, 2);
    assert_eq!(out.steps, 2);
    assert_eq!(probe.max_in_flight(), 2, "read-only calls must overlap");

    let results = last_tool_results(&provider.requests()[1]);
    assert_eq!(
        results,
        vec![
            ("c1".into(), "first".into(), false),
            ("c2".into(), "second".into(), false),
        ]
    );
    // The assistant turn is kept before the results.
    let second = &provider.requests()[1];
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[1].role, Role::Assistant);
    assert!(second.messages[1].has_tool_use());
}

#[tokio::test]
async fn mutating_tools_run_sequentially_in_order() {
    let probe = ConcurrencyProbe::new();
    let tools = ToolRegistry::new()
        .with(Arc::new(SlowMutatingTool::new(
            Duration::from_millis(20),
            probe.clone(),
        )))
        .with(Arc::new(
            EchoTool::new()
                .with_delay(Duration::from_millis(20))
                .with_probe(probe.clone()),
        ));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![
            ("m1", "mutate", json!({"text": "a"})),
            ("m2", "mutate", json!({"text": "b"})),
            ("e1", "echo", json!({"text": "c"})),
            ("m3", "mutate", json!({"text": "d"})),
        ]),
        text_response("done"),
    ]));
    let out = runner(provider.clone(), tools)
        .run(&spec(), "go".into())
        .await
        .unwrap();

    assert_eq!(out.tool_calls, 4);
    assert_eq!(probe.max_in_flight(), 1, "mutating calls must not overlap");
    assert_eq!(probe.order(), vec!["a", "b", "c", "d"]);
    let results = last_tool_results(&provider.requests()[1]);
    let ids: Vec<&str> = results.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(ids, vec!["m1", "m2", "e1", "m3"]);
    assert_eq!(results[0].1, "wrote a");
    assert_eq!(results[2].1, "c");
}

#[tokio::test]
async fn max_steps_stop() {
    let tools = ToolRegistry::new().with(Arc::new(EchoTool::new()));
    let provider = Arc::new(ScriptedProvider::new(
        (0..5)
            .map(|i| tool_use_response(vec![(&*format!("c{i}"), "echo", json!({"text": "x"}))]))
            .collect(),
    ));
    let out = runner(provider.clone(), tools)
        .run(&spec().with_max_steps(3), "loop forever".into())
        .await
        .unwrap();

    assert_eq!(out.stop, AgentStop::MaxSteps);
    assert!(!out.is_success());
    assert_eq!(out.steps, 3);
    assert_eq!(out.tool_calls, 3);
    assert_eq!(provider.request_count(), 3);
    // Transcript ends with the results of the last executed call.
    assert_eq!(out.messages.last().unwrap().role, Role::User);
}

#[tokio::test]
async fn hook_veto_becomes_error_result_seen_by_model() {
    let hook = Arc::new(VetoHook::new("echo", "shell access is disabled"));
    let mut registry = Registry::new();
    registry.add_hook(hook.clone());
    let tools = ToolRegistry::new().with(Arc::new(EchoTool::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "echo", json!({"text": "hi"}))]),
        text_response("ok, I will do something else"),
    ]));
    let out = runner_with_registry(provider.clone(), tools, registry)
        .run(&spec(), "go".into())
        .await
        .unwrap();

    assert_eq!(out.stop, AgentStop::Completed);
    let results = last_tool_results(&provider.requests()[1]);
    assert_eq!(results.len(), 1);
    let (id, content, is_error) = &results[0];
    assert_eq!(id, "c1");
    assert!(is_error);
    assert!(content.contains("blocked by a policy hook"), "{content}");
    assert!(
        content.contains("veto: shell access is disabled"),
        "{content}"
    );
    assert_eq!(hook.after_calls(), 0, "a vetoed tool never runs");
}

#[tokio::test]
async fn context_window_warning_injection_then_stop() {
    // ~8 600 tokens of tool output in a 10 000 token window: past 85%, below 90%.
    let tools = ToolRegistry::new().with(Arc::new(BigOutputTool::new(34_400)));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "big", json!({}))]),
        tool_use_response(vec![("c2", "big", json!({}))]),
        text_response("never reached"),
    ]));
    let out = runner(provider.clone(), tools)
        .context_window(10_000)
        .run(&spec(), "read everything".into())
        .await
        .unwrap();

    assert_eq!(out.stop, AgentStop::ContextWindow);
    assert_eq!(out.steps, 2);
    let reqs = provider.requests();
    assert_eq!(reqs.len(), 2);
    assert!(!last_user_text(&reqs[0]).contains(CONTEXT_WARNING_MESSAGE));
    // The warning is merged into the user turn that carries the tool results.
    let last = reqs[1].messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    assert!(
        last.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    );
    assert!(last.text().contains(CONTEXT_WARNING_MESSAGE));
    let warnings = out
        .messages
        .iter()
        .filter(|m| m.text().contains(CONTEXT_WARNING_MESSAGE))
        .count();
    assert_eq!(warnings, 1, "the warning is injected once");
}

#[derive(Debug, serde::Deserialize, PartialEq)]
struct Report {
    status: String,
    files_changed: Vec<String>,
}

#[tokio::test]
async fn structured_output_from_fenced_json() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "All done.\n```json\n{\"status\": \"done\", \"files_changed\": [\"a.rs\"]}\n```",
    )]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let (report, outcome): (Report, _) =
        run_structured(&r, &spec().with_structured_output(), "go".into())
            .await
            .unwrap();
    assert_eq!(
        report,
        Report {
            status: "done".into(),
            files_changed: vec!["a.rs".into()]
        }
    );
    assert!(outcome.is_success());
    assert_eq!(provider.request_count(), 1);
}

#[tokio::test]
async fn structured_output_from_unfenced_json() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "I changed {one file}. Result: {\"status\": \"failed\", \"files_changed\": []} -- end",
    )]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let (report, _): (Report, _) = run_structured(&r, &spec(), "go".into()).await.unwrap();
    assert_eq!(report.status, "failed");
    assert_eq!(provider.request_count(), 1);
}

#[tokio::test]
async fn structured_output_json_repair_path() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("status: done, files: a.rs"),
        text_response("{\"status\": \"done\", \"files_changed\": [\"a.rs\"]}"),
    ]));
    let r = runner(
        provider.clone(),
        ToolRegistry::new().with(Arc::new(EchoTool::new())),
    );
    let (report, outcome): (Report, _) = run_structured(&r, &spec(), "go".into()).await.unwrap();
    assert_eq!(report.files_changed, vec!["a.rs"]);
    assert_eq!(outcome.steps, 1, "the repair call is not an agent step");

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 2);
    let repair = &reqs[1];
    assert!(repair.tools.is_empty(), "repair runs without tools");
    assert!(repair.system.contains("valid JSON"));
    assert!(last_user_text(repair).contains("status: done, files: a.rs"));
}

#[tokio::test]
async fn structured_output_retries_agent_after_failed_repair() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("no json at all"),
        text_response("still not json"),
        text_response("{\"status\": \"done\", \"files_changed\": []}"),
    ]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let (report, outcome): (Report, _) = run_structured(&r, &spec(), "go".into()).await.unwrap();
    assert_eq!(report.status, "done");
    assert_eq!(outcome.steps, 2);
    let reqs = provider.requests();
    assert_eq!(reqs.len(), 3);
    let retry = &reqs[2];
    assert!(last_user_text(retry).starts_with("Your previous answer was not valid JSON:"));
    // The retry continues the original transcript.
    assert_eq!(retry.messages[0], Message::user("go"));
    assert_eq!(retry.messages[1], Message::assistant("no json at all"));
}

#[tokio::test]
async fn structured_output_gives_up_after_one_retry() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("nope"),
        text_response("nope again"),
        text_response("still nope"),
    ]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let err = run_structured::<Report>(&r, &spec(), "go".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
    assert_eq!(provider.request_count(), 3);
}

#[tokio::test]
async fn structured_output_does_not_retry_failed_runs() {
    let provider = Arc::new(ScriptedProvider::with_results(vec![Err(Error::new(
        ErrorKind::AuthFailed,
        "bad key",
    ))]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let err = run_structured::<Report>(&r, &spec(), "go".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthFailed);
    assert_eq!(provider.request_count(), 1);
}

#[tokio::test]
async fn continuation_summary_starts_a_fresh_session() {
    // 40 000 chars ~ 10 000 tokens: the first session overflows a 10k window.
    let tools = ToolRegistry::new().with(Arc::new(BigOutputTool::new(40_000)));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "big", json!({}))]),
        text_response("Goal: X. Done: read big output. Next: finish."),
        text_response("finished"),
    ]));
    let r = runner(provider.clone(), tools).context_window(10_000);
    let out = run_with_continuation(
        &r,
        &spec(),
        "Implement X".into(),
        ContinuationPolicy::default(),
    )
    .await
    .unwrap();

    assert_eq!(out.stop, AgentStop::Completed);
    assert_eq!(out.final_text, "finished");
    assert_eq!(out.steps, 2);
    assert_eq!(out.tool_calls, 1);

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 3);
    let summary_req = &reqs[1];
    assert!(summary_req.tools.is_empty());
    assert!(summary_req.system.contains("handover"));
    assert!(last_user_text(summary_req).contains("[ASSISTANT called big]"));

    let fresh = &reqs[2];
    assert_eq!(fresh.messages.len(), 1, "a fresh session");
    let first = last_user_text(fresh);
    assert!(first.starts_with("Implement X"));
    assert!(first.contains(
        "Summary of your previous session:\nGoal: X. Done: read big output. Next: finish."
    ));
    assert!(first.ends_with("Continue where you left off."));
    // Both sessions are concatenated in the outcome.
    assert_eq!(out.messages.len(), 3 + 2);
}

#[tokio::test]
async fn continuation_respects_the_limit() {
    let tools = ToolRegistry::new().with(Arc::new(BigOutputTool::new(40_000)));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "big", json!({}))]),
        text_response("summary one"),
        tool_use_response(vec![("c2", "big", json!({}))]),
    ]));
    let r = runner(provider.clone(), tools).context_window(10_000);
    let out = run_with_continuation(
        &r,
        &spec(),
        "Implement X".into(),
        ContinuationPolicy::new(1),
    )
    .await
    .unwrap();
    assert_eq!(out.stop, AgentStop::ContextWindow);
    assert_eq!(out.steps, 2);
    assert_eq!(provider.request_count(), 3);

    let none = Arc::new(ScriptedProvider::new(vec![tool_use_response(vec![(
        "c1",
        "big",
        json!({}),
    )])]));
    let r = runner(
        none.clone(),
        ToolRegistry::new().with(Arc::new(BigOutputTool::new(40_000))),
    )
    .context_window(10_000);
    let out = run_with_continuation(&r, &spec(), "X".into(), ContinuationPolicy::new(0))
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::ContextWindow);
    assert_eq!(none.request_count(), 1);
}

// --------------------------------------------------------------- edge cases

#[tokio::test]
async fn unknown_failing_and_panicking_tools_become_error_results() {
    let tools = ToolRegistry::new()
        .with(Arc::new(FailingTool::error()))
        .with(Arc::new(FailingTool::panicking()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![
            ("c1", "nope", json!({})),
            ("c2", "fail", json!({})),
            ("c3", "panic", json!({})),
        ]),
        text_response("recovered"),
    ]));
    let out = runner(provider.clone(), tools)
        .run(&spec(), "go".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::Completed);
    let results = last_tool_results(&provider.requests()[1]);
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r.2));
    assert!(results[0].1.contains("Unknown tool `nope`"));
    assert!(results[0].1.contains("fail, panic"));
    assert!(results[1].1.contains("boom"));
    assert!(results[2].1.contains("panicked on purpose"));
}

#[tokio::test]
async fn tool_selection_limits_offered_tools() {
    let tools = ToolRegistry::new()
        .with(Arc::new(EchoTool::new()))
        .with(Arc::new(EchoTool::new().named("read_file")));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "echo", json!({"text": "x"}))]),
        text_response("ok"),
    ]));
    let spec = spec().with_tools(ToolSelection::ReadOnly);
    runner(provider.clone(), tools)
        .run(&spec, "go".into())
        .await
        .unwrap();
    let reqs = provider.requests();
    let offered: Vec<&str> = reqs[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(offered, vec!["read_file"]);
    let results = last_tool_results(&reqs[1]);
    assert!(results[0].2);
    assert!(results[0].1.contains("Unknown tool `echo`"));
}

#[tokio::test]
async fn long_tool_output_is_truncated_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let tools = ToolRegistry::new().with(Arc::new(BigOutputTool::new(1_000)));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "big", json!({}))]),
        text_response("ok"),
    ]));
    runner(provider.clone(), tools)
        .tool_context(ToolContext::new(dir.path()))
        .max_tool_output_chars(100)
        .run(&spec(), "go".into())
        .await
        .unwrap();
    let results = last_tool_results(&provider.requests()[1]);
    let content = &results[0].1;
    assert!(content.starts_with(&"x".repeat(100)));
    assert!(!content.starts_with(&"x".repeat(101)));
    assert!(content.contains("showing the first 100 of 1000 characters"));

    let saved_dir = dir.path().join(".vibe").join("tool-output");
    let files: Vec<_> = std::fs::read_dir(&saved_dir).unwrap().collect();
    assert_eq!(files.len(), 1);
    let path = files[0].as_ref().unwrap().path();
    assert_eq!(path.extension().unwrap(), "txt");
    assert!(content.contains(&path.display().to_string()));
    assert_eq!(std::fs::read_to_string(path).unwrap().len(), 1_000);
}

#[tokio::test]
async fn cancellation_before_start_and_between_steps() {
    let (tx, rx) = tokio::sync::watch::channel(true);
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("x")]));
    let out = runner(provider.clone(), ToolRegistry::new())
        .cancel_token(rx)
        .run(&spec(), "go".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::Cancelled);
    assert_eq!(out.steps, 0);
    assert_eq!(provider.request_count(), 0);
    drop(tx);

    // A tool flips the token: the run stops before the next step.
    struct Canceller(tokio::sync::watch::Sender<bool>);
    #[async_trait::async_trait]
    impl Tool for Canceller {
        fn name(&self) -> &str {
            "cancel"
        }
        fn description(&self) -> &str {
            "cancel the run"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn call(
            &self,
            _ctx: &ToolContext,
            _input: serde_json::Value,
        ) -> vibe_core::Result<ToolOutput> {
            let _ = self.0.send(true);
            Ok(ToolOutput::ok("cancelling"))
        }
    }
    let (tx, rx) = tokio::sync::watch::channel(false);
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "cancel", json!({}))]),
        text_response("never"),
    ]));
    let out = runner(
        provider.clone(),
        ToolRegistry::new().with(Arc::new(Canceller(tx))),
    )
    .cancel_token(rx)
    .run(&spec(), "go".into())
    .await
    .unwrap();
    assert_eq!(out.stop, AgentStop::Cancelled);
    assert_eq!(out.steps, 1);
    assert_eq!(provider.request_count(), 1);
}

#[tokio::test]
async fn provider_errors_end_the_run() {
    let provider = Arc::new(ScriptedProvider::with_results(vec![Err(Error::new(
        ErrorKind::AuthFailed,
        "invalid key",
    ))]));
    let out = runner(provider, ToolRegistry::new())
        .run(&spec(), "go".into())
        .await
        .unwrap();
    assert_eq!(
        out.stop,
        AgentStop::Error {
            kind: ErrorKind::AuthFailed,
            message: "invalid key".into()
        }
    );
    assert_eq!(out.steps, 0);
    assert_eq!(out.messages.len(), 1, "the transcript is kept");

    let provider = Arc::new(ScriptedProvider::with_results(vec![Err(Error::new(
        ErrorKind::ContextTooLong,
        "prompt too long",
    ))]));
    let out = runner(provider, ToolRegistry::new())
        .run(&spec(), "go".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::ContextWindow);
}

#[tokio::test]
async fn retryable_provider_errors_are_retried() {
    let provider = Arc::new(ScriptedProvider::with_results(vec![
        Err(Error::new(ErrorKind::RateLimited, "slow down").with_retry_after(Duration::ZERO)),
        Err(Error::new(ErrorKind::ServerError, "502")),
        Ok(text_response("ok")),
    ]));
    let r = runner(provider.clone(), ToolRegistry::new());
    let mut rx = r.events().subscribe();
    let out = r.run(&spec(), "go".into()).await.unwrap();
    assert_eq!(out.stop, AgentStop::Completed);
    assert_eq!(out.steps, 1);
    assert_eq!(provider.request_count(), 3);
    let mut retries = 0;
    while let Ok(env) = rx.try_recv() {
        if matches!(env.event, Event::Retrying { .. }) {
            retries += 1;
        }
    }
    assert_eq!(retries, 2);

    // Retries are bounded.
    let provider = Arc::new(ScriptedProvider::with_results(vec![
        Err(Error::new(ErrorKind::Network, "reset")),
        Err(Error::new(ErrorKind::Network, "reset")),
    ]));
    let out = runner(provider.clone(), ToolRegistry::new())
        .max_provider_retries(1)
        .run(&spec(), "go".into())
        .await
        .unwrap();
    assert!(matches!(
        out.stop,
        AgentStop::Error {
            kind: ErrorKind::Network,
            ..
        }
    ));
    assert_eq!(provider.request_count(), 2);
}

#[tokio::test]
async fn max_tokens_without_tool_use_nudges_once() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        response_with_stop("part one", StopReason::MaxTokens),
        response_with_stop("part two", StopReason::MaxTokens),
        text_response("never"),
    ]));
    let out = runner(provider.clone(), ToolRegistry::new())
        .run(&spec(), "write a lot".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::Completed);
    assert_eq!(out.steps, 2);
    assert_eq!(out.final_text, "part two");
    let reqs = provider.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(last_user_text(&reqs[1]), CONTINUE_NUDGE);
}

#[tokio::test]
async fn reviewers_are_told_to_converge_at_75_percent() {
    let tools = ToolRegistry::new().with(Arc::new(EchoTool::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "echo", json!({"text": "1"}))]),
        tool_use_response(vec![("c2", "echo", json!({"text": "2"}))]),
        tool_use_response(vec![("c3", "echo", json!({"text": "3"}))]),
        text_response("{\"verdict\": \"approved\"}"),
    ]));
    let spec = AgentSpec::new(AgentRole::QaReviewer, "Review.").with_max_steps(4);
    let out = runner(provider.clone(), tools)
        .run(&spec, "review".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::Completed);
    let reqs = provider.requests();
    for req in &reqs[..3] {
        assert!(!last_user_text(req).contains(CONVERGE_MESSAGE));
    }
    assert!(last_user_text(&reqs[3]).contains(CONVERGE_MESSAGE));
}

#[tokio::test]
async fn coders_are_not_told_to_converge() {
    let tools = ToolRegistry::new().with(Arc::new(EchoTool::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "echo", json!({"text": "1"}))]),
        text_response("done"),
    ]));
    let spec = AgentSpec::new(AgentRole::Coder, "Code.").with_max_steps(2);
    runner(provider.clone(), tools)
        .run(&spec, "code".into())
        .await
        .unwrap();
    assert!(!last_user_text(&provider.requests()[1]).contains(CONVERGE_MESSAGE));
}

#[tokio::test]
async fn system_prompt_variables_and_hook_augmentation() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = Registry::new();
    registry.add_hook(Arc::new(PromptHook("Project rule: use tabs.".into())));
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let spec = AgentSpec::new(
        AgentRole::Planner,
        "<!--\n- task_title: t\n-->\nT={{task_title}} D={{task_description}} \
         W={{workspace_root}} S={{spec}} M={{missing}} Y={{date}}",
    )
    .with_thinking(ThinkingLevel::High);
    runner_with_registry(provider.clone(), ToolRegistry::new(), registry)
        .task(Task::new("Title", "Body"))
        .tool_context(ToolContext::new(dir.path()))
        .var("spec", "SPEC")
        .run(&spec, "plan".into())
        .await
        .unwrap();

    let req = &provider.requests()[0];
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(
        req.system,
        format!(
            "T=Title D=Body W={} S=SPEC M= Y={today}\n\nProject rule: use tabs.",
            dir.path().display()
        )
    );
    assert_eq!(req.thinking_budget, Some(16_384));
    assert!(req.max_tokens > 16_384);
}

#[tokio::test]
async fn events_are_published() {
    let tools = ToolRegistry::new().with(Arc::new(EchoTool::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_use_response(vec![("c1", "echo", json!({"text": "hi"}))]),
        text_response("done"),
    ]));
    let r = runner(provider, tools);
    let mut rx = r.events().subscribe();
    r.run(&spec(), "go".into()).await.unwrap();
    let mut kinds = Vec::new();
    while let Ok(env) = rx.try_recv() {
        assert_eq!(env.event.run_id(), Some(r.current_run_id()));
        kinds.push(match env.event {
            Event::AgentStarted { .. } => "started",
            Event::ToolCalled { tool, .. } => {
                assert_eq!(tool, "echo");
                "called"
            }
            Event::ToolReturned {
                is_error, preview, ..
            } => {
                assert!(!is_error);
                assert_eq!(preview, "hi");
                "returned"
            }
            Event::AgentText { text, .. } => {
                assert_eq!(text, "done");
                "text"
            }
            Event::AgentFinished { steps, stop, .. } => {
                assert_eq!(steps, 2);
                assert!(stop.contains("completed"));
                "finished"
            }
            _ => "other",
        });
    }
    assert_eq!(
        kinds,
        vec!["started", "called", "returned", "text", "finished"]
    );
}

#[tokio::test]
async fn resume_closes_dangling_tool_calls() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let history = vec![
        Message::user("go"),
        tool_use_response(vec![("c1", "echo", json!({}))]).message,
    ];
    let out = runner(provider.clone(), ToolRegistry::new())
        .resume(&spec(), history, "please finish".into())
        .await
        .unwrap();
    assert_eq!(out.stop, AgentStop::Completed);
    let req = &provider.requests()[0];
    assert_eq!(req.messages.len(), 3);
    let results = last_tool_results(req);
    assert_eq!(results[0].0, "c1");
    assert!(results[0].2);
    assert_eq!(last_user_text(req), "please finish");
}

#[tokio::test]
async fn builtin_agents_run_with_the_loop() {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "```json\n{\"complexity\": \"simple\", \"confidence\": 0.8, \"reasoning\": \"r\", \
         \"needs_research\": false, \"needs_critique\": false, \"risk_level\": \"low\"}\n```",
    )]));
    let spec = vibe_agents::builtin_agent(&AgentRole::ComplexityAssessor).unwrap();
    let r = runner(provider.clone(), ToolRegistry::new()).task(Task::new("Fix typo", "In README"));
    let (value, _): (serde_json::Value, _) =
        run_structured(&r, &spec, "Assess".into()).await.unwrap();
    assert_eq!(value["complexity"], "simple");
    let system = &provider.requests()[0].system;
    assert!(system.contains("**Fix typo**"));
    assert!(!system.contains("<!--"));
    assert!(!system.contains("{{"));
}
