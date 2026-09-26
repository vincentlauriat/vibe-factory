//! End-to-end tests driving the real `vibe-echo-plugin` process.

use std::time::{Duration, Instant};

use serde_json::json;
use vibe_core::config::PluginConfig;
use vibe_core::{AgentRole, HookDecision, Registry, ToolContext, ToolOutput, ToolSelection};
use vibe_plugins::discovery::{discover_in, merge_with_config};
use vibe_plugins::{MANIFEST_FILE, PluginHost};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_vibe-echo-plugin");

fn echo_config(required: bool) -> PluginConfig {
    PluginConfig {
        name: "echo".into(),
        command: vec![ECHO_BIN.into()],
        env: Default::default(),
        cwd: None,
        capabilities: Vec::new(),
        required,
    }
}

#[tokio::test]
async fn real_process_contributes_tools_agents_and_hooks() {
    let mut host = PluginHost::load(&[echo_config(true)]).await.unwrap();

    let info = &host.plugins()[0];
    assert_eq!(info.name, "echo");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools && info.capabilities.agents && info.capabilities.hooks);
    assert_eq!(info.tool_names, vec!["echo"]);
    assert!(!info.native);

    let mut registry = Registry::new();
    host.register_all(&mut registry).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path());
    let tool = registry.tools.get("echo").expect("echo tool registered");
    assert_eq!(tool.input_schema()["required"], json!(["text"]));
    let out = tool.call(&ctx, json!({"text": "hello"})).await.unwrap();
    assert_eq!(out, ToolOutput::ok("hello"));
    // The tool context travels to the plugin as `_meta.vibe`.
    let seen = tool.call(&ctx, json!({"workspace": true})).await.unwrap();
    assert_eq!(seen.content, dir.path().display().to_string());
    let err = tool.call(&ctx, json!({})).await.unwrap();
    assert!(err.is_error);
    assert!(err.content.contains("text"));

    // Concurrent calls are multiplexed over one process.
    let calls = (0..8).map(|i| {
        let tool = std::sync::Arc::clone(tool);
        let ctx = ctx.clone();
        async move { tool.call(&ctx, json!({"text": i.to_string()})).await }
    });
    let results = futures::future::join_all(calls).await;
    for (i, r) in results.into_iter().enumerate() {
        assert_eq!(r.unwrap().content, i.to_string());
    }

    let agent = registry
        .agent(&AgentRole::Custom("echo_agent".into()))
        .expect("agent registered");
    assert_eq!(agent.tools, ToolSelection::Named(vec!["echo".into()]));

    assert!(matches!(
        registry.before_tool(&ctx, "forbidden", &json!({})).await,
        HookDecision::Abort(reason) if reason.contains("forbids")
    ));
    assert_eq!(
        registry.before_tool(&ctx, "echo", &json!({})).await,
        HookDecision::Continue
    );

    let started = Instant::now();
    host.shutdown_all().await;
    assert!(host.plugins().is_empty());
    // A graceful exit must not need the kill path.
    assert!(started.elapsed() < Duration::from_millis(1900));
    // The tool now fails cleanly instead of hanging.
    assert!(tool.call(&ctx, json!({"text": "late"})).await.is_err());
}

#[tokio::test]
async fn manifest_discovery_loads_the_plugin() {
    let root = tempfile::tempdir().unwrap();
    let plugin_dir = root.path().join("echo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let command = toml::Value::Array(vec![toml::Value::String(ECHO_BIN.into())]);
    std::fs::write(
        plugin_dir.join(MANIFEST_FILE),
        format!(
            "name = \"echo-from-manifest\"\nversion = \"0.1.0\"\ncommand = {command}\nrequired = true\n"
        ),
    )
    .unwrap();

    let discovered = discover_in(&[root.path().to_path_buf()]);
    assert_eq!(discovered.len(), 1);
    let configs = merge_with_config(&discovered, &vibe_core::VibeConfig::default());
    let mut host = PluginHost::load(&configs).await.unwrap();
    assert_eq!(host.plugins()[0].name, "echo-from-manifest");
    host.shutdown_all().await;
}

#[tokio::test]
async fn broken_optional_plugin_does_not_block_others() {
    let broken = PluginConfig {
        name: "broken".into(),
        command: vec!["vibe-plugin-that-does-not-exist-anywhere".into()],
        env: Default::default(),
        cwd: None,
        capabilities: Vec::new(),
        required: false,
    };
    let mut host = PluginHost::load(&[broken, echo_config(true)])
        .await
        .unwrap();
    let names: Vec<_> = host.plugins().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["echo"]);
    host.shutdown_all().await;
}

#[tokio::test]
async fn relative_program_resolves_against_cwd() {
    let bin = std::path::Path::new(ECHO_BIN);
    let file_name = bin.file_name().unwrap().to_string_lossy().into_owned();
    let config = PluginConfig {
        name: "echo-relative".into(),
        command: vec![format!(".{}{file_name}", std::path::MAIN_SEPARATOR)],
        env: Default::default(),
        cwd: bin.parent().map(std::path::Path::to_path_buf),
        capabilities: vec!["tools".into()],
        required: true,
    };
    let mut host = PluginHost::load(&[config]).await.unwrap();
    assert_eq!(host.plugins()[0].tool_names, vec!["echo"]);
    host.shutdown_all().await;
}
