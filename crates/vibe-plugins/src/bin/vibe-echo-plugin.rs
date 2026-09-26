//! Example plugin speaking the Vibe Plugin Protocol on stdio.
//!
//! It offers:
//! - a tool `echo` returning its `text` argument (with
//!   `"workspace": true`, it returns the workspace root received from the
//!   host in `_meta.vibe` instead, or `<none>`);
//! - an agent spec with the custom role `echo_agent`;
//! - a `before_tool` hook aborting every call to a tool named `forbidden`.

use serde_json::{Value, json};
use vibe_core::{AgentRole, AgentSpec, Error, HookDecision, ToolOutput, ToolSelection};
use vibe_plugins::{PluginServer, ToolCallContext};

#[tokio::main]
async fn main() {
    let server = PluginServer::new("echo", env!("CARGO_PKG_VERSION"))
        .tool_with_context(
            "echo",
            "Return the given text unchanged.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to echo" },
                    "workspace": {
                        "type": "boolean",
                        "description": "Return the workspace root instead"
                    }
                },
                "required": ["text"]
            }),
            |args: Value, ctx: Option<ToolCallContext>| async move {
                if args.get("workspace").and_then(Value::as_bool) == Some(true) {
                    return Ok(ToolOutput::ok(match ctx {
                        Some(c) => c.workspace_root.display().to_string(),
                        None => "<none>".to_string(),
                    }));
                }
                match args.get("text").and_then(Value::as_str) {
                    Some(text) => Ok(ToolOutput::ok(text)),
                    None => Err(Error::tool("missing string argument `text`")),
                }
            },
        )
        .agent(
            AgentSpec::new(
                AgentRole::Custom("echo_agent".into()),
                "You repeat what you are told using the `echo` tool.",
            )
            .with_description("Echoes its instructions")
            .with_tools(ToolSelection::Named(vec!["echo".into()])),
        )
        .before_tool(|tool: String, _input: Value| async move {
            if tool == "forbidden" {
                HookDecision::Abort("the echo plugin forbids this tool".into())
            } else {
                HookDecision::Continue
            }
        });
    let code = match server.run_stdio().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("vibe-echo-plugin: {e}");
            1
        }
    };
    // Exit right away: the runtime would otherwise wait for the blocking
    // stdin reader thread.
    std::process::exit(code);
}
