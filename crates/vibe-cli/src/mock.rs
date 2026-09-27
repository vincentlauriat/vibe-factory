//! The `mock` provider of the CLI: canned, role-aware answers for trying the
//! pipeline without an API key, and scripted answers loaded with
//! `vibe run --script <file.json>`.
//!
//! ## Script format
//!
//! Either a JSON array of steps, replayed in order:
//!
//! ```json
//! ["plain text answer", {"json": {"verdict": "approved", "issues": []}}]
//! ```
//!
//! or an object:
//!
//! ```json
//! {
//!   "routes": {"planner": [{"json": {"approach": "…", "phases": []}}]},
//!   "steps": ["answer for any role"],
//!   "fallback": true
//! }
//! ```
//!
//! A request is answered by the next response routed to its agent role
//! (recognised from the system prompt), else by the next step, else — when
//! `fallback` is true (the default) — by the built-in canned answer for the
//! role. A step is a string (text answer), `{"text": "…"}`, `{"json": …}`
//! (a fenced JSON document, as structured agents produce),
//! `{"tool": "name", "input": {…}}` (one tool call), or a full
//! `CompletionResponse` object.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::json;
use vibe_core::{
    AgentSpec, CompletionRequest, CompletionResponse, ContentBlock, Error, Message, Role,
    StopReason, Usage,
};
use vibe_providers::MockProvider;
use vibe_providers::mock::text_response;

/// One scripted answer.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum ScriptStep {
    /// A text answer.
    Text(String),
    /// A full response.
    Full(CompletionResponse),
    /// `{"json": …}`: a fenced JSON document.
    Json {
        /// Document.
        json: serde_json::Value,
    },
    /// `{"tool": "name", "input": {…}}`: one tool call.
    Tool {
        /// Tool name.
        tool: String,
        /// Tool arguments.
        #[serde(default)]
        input: serde_json::Value,
    },
    /// `{"text": "…"}`.
    TextObject {
        /// Text.
        text: String,
    },
}

/// Content of a script file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Script {
    /// Answers for any role, in order.
    #[serde(default)]
    pub steps: Vec<ScriptStep>,
    /// Answers per agent role name, in order.
    #[serde(default)]
    pub routes: BTreeMap<String, Vec<ScriptStep>>,
    /// Use the canned answers once the script is exhausted.
    #[serde(default = "default_true")]
    pub fallback: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Script {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            routes: BTreeMap::new(),
            fallback: true,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ScriptFile {
    Steps(Vec<ScriptStep>),
    Full(Script),
}

impl Script {
    /// Parse a script from JSON text.
    pub fn from_json(text: &str) -> Result<Self> {
        let file: ScriptFile = serde_json::from_str(text).context("invalid script")?;
        Ok(match file {
            ScriptFile::Steps(steps) => Script {
                steps,
                routes: BTreeMap::new(),
                fallback: true,
            },
            ScriptFile::Full(s) => s,
        })
    }

    /// Load a script file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read script {}", path.display()))?;
        Self::from_json(&text).with_context(|| format!("in script {}", path.display()))
    }
}

/// Recognises the agent role of a request from its system prompt.
#[derive(Debug, Clone, Default)]
pub struct RoleMatcher {
    markers: Vec<(String, String)>,
}

impl RoleMatcher {
    /// Markers taken from the first line of each agent's prompt (up to the
    /// first `{{` placeholder).
    pub fn from_agents<'a>(agents: impl IntoIterator<Item = &'a AgentSpec>) -> Self {
        let mut markers: Vec<(String, String)> = agents
            .into_iter()
            .filter_map(|spec| {
                let prompt = vibe_agents::strip_doc_comment(&spec.system_prompt);
                let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
                let marker = line.split("{{").next().unwrap_or("").trim();
                (marker.chars().count() >= 12).then(|| (marker.to_string(), spec.role.name()))
            })
            .collect();
        // Longest markers first, so that a marker that is a prefix of
        // another never wins.
        markers.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
        Self { markers }
    }

    /// Role whose marker appears in `system`.
    pub fn role_of(&self, system: &str) -> Option<&str> {
        self.markers
            .iter()
            .find(|(m, _)| system.contains(m.as_str()))
            .map(|(_, r)| r.as_str())
    }
}

struct ScriptState {
    steps: VecDeque<ScriptStep>,
    routes: BTreeMap<String, VecDeque<ScriptStep>>,
    fallback: bool,
}

/// Build the CLI mock provider: `script` first (when given), then canned
/// answers.
pub fn mock_provider(script: Option<Script>, matcher: RoleMatcher) -> MockProvider {
    let script = script.unwrap_or_default();
    let state = Mutex::new(ScriptState {
        steps: script.steps.into(),
        routes: script
            .routes
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
        fallback: script.fallback,
    });
    let ids = AtomicU64::new(1);
    MockProvider::new()
        .with_name("mock")
        .with_handler(move |request: &CompletionRequest| {
            let role = matcher.role_of(&request.system).unwrap_or("unknown");
            let step = {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                let routed = st.routes.get_mut(role).and_then(VecDeque::pop_front);
                match routed.or_else(|| st.steps.pop_front()) {
                    Some(step) => Some(step),
                    None if st.fallback => None,
                    None => {
                        return Err(Error::other(format!(
                            "mock script exhausted (request from role `{role}`)"
                        )));
                    }
                }
            };
            Ok(match step {
                Some(step) => step_response(step, &ids),
                None => canned_response(role),
            })
        })
}

fn step_response(step: ScriptStep, ids: &AtomicU64) -> CompletionResponse {
    match step {
        ScriptStep::Text(text) | ScriptStep::TextObject { text } => text_response(text),
        ScriptStep::Full(response) => response,
        ScriptStep::Json { json } => fenced(&json),
        ScriptStep::Tool { tool, input } => {
            let n = ids.fetch_add(1, Ordering::Relaxed);
            CompletionResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: format!("script_call_{n}"),
                        name: tool,
                        input: if input.is_null() { json!({}) } else { input },
                    }],
                },
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
                model: String::new(),
            }
        }
    }
}

/// A text answer holding `value` in a ```json fence.
pub fn fenced(value: &serde_json::Value) -> CompletionResponse {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    text_response(format!("Here is my answer.\n\n```json\n{pretty}\n```"))
}

/// Canned answer for a role, valid for the pipeline's structured outputs.
pub fn canned_response(role: &str) -> CompletionResponse {
    let spec = json!({
        "summary": "Specification produced by the mock provider (no model was called).",
        "requirements": [{
            "id": "R1",
            "description": "Implement the task as described.",
            "kind": "functional",
            "priority": 1,
            "acceptance": ["The change is implemented and the project still builds."]
        }],
        "context": {"relevant_files": [], "findings": [], "assumptions": ["Mock run."]}
    });
    match role {
        "complexity_assessor" => fenced(&json!({
            "complexity": "simple",
            "confidence": 0.5,
            "reasoning": "Fixed answer of the mock provider.",
            "needs_research": false,
            "needs_critique": false,
            "risk_level": "low"
        })),
        "spec_gatherer" | "spec_writer" => fenced(&spec),
        "spec_critic" => fenced(&json!({"verdict": "approved", "issues": []})),
        "spec_researcher" => text_response("No external research needed (mock provider)."),
        "planner" => fenced(&json!({
            "approach": "Single step plan produced by the mock provider.",
            "phases": [{"name": "Implementation", "subtasks": [{
                "title": "Implement the task",
                "description": "Make the change described by the task.",
                "files": [],
                "verification": []
            }]}]
        })),
        "coder" | "coder_recovery" => fenced(&json!({
            "status": "done",
            "summary": "Nothing changed: the mock provider does not edit files.",
            "files_changed": [],
            "notes": ""
        })),
        "qa_reviewer" => fenced(&json!({
            "verdict": "approved",
            "summary": "Approved by the mock provider.",
            "issues": []
        })),
        "qa_fixer" => fenced(&json!({"status": "done", "summary": "Nothing to fix.", "fixed": []})),
        "commit_message" => text_response("vibe: mock change"),
        _ => text_response("Mock response."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibe_core::ModelProvider;

    fn matcher() -> RoleMatcher {
        let agents = vibe_agents::builtin_agents();
        RoleMatcher::from_agents(agents.iter())
    }

    fn request(system: &str) -> CompletionRequest {
        let mut r = CompletionRequest::new("m", vec![Message::user("go")]);
        r.system = system.to_string();
        r
    }

    #[test]
    fn roles_are_recognised_from_builtin_prompts() {
        let m = matcher();
        for spec in vibe_agents::builtin_agents() {
            let rendered = spec.system_prompt.replace("{{date}}", "2026-01-01");
            assert_eq!(
                m.role_of(&rendered),
                Some(spec.role.name().as_str()),
                "{}",
                spec.role
            );
        }
        assert_eq!(m.role_of("hello"), None);
    }

    #[test]
    fn script_shapes_parse() {
        let s = Script::from_json(r#"["a", {"text": "b"}, {"json": {"x": 1}}, {"tool": "read_file", "input": {"path": "x"}}]"#).unwrap();
        assert_eq!(s.steps.len(), 4);
        assert!(matches!(s.steps[3], ScriptStep::Tool { .. }));
        let s = Script::from_json(r#"{"routes": {"planner": ["p"]}, "fallback": false}"#).unwrap();
        assert!(!s.fallback);
        assert_eq!(s.routes["planner"].len(), 1);
        let full = serde_json::to_string(&text_response("hi")).unwrap();
        let s = Script::from_json(&format!("[{full}]")).unwrap();
        assert!(matches!(s.steps[0], ScriptStep::Full(_)));
    }

    #[tokio::test]
    async fn routes_then_steps_then_canned() {
        let planner = vibe_agents::builtin_agent(&vibe_core::AgentRole::Planner).unwrap();
        let script =
            Script::from_json(r#"{"routes": {"planner": ["routed"]}, "steps": ["step"]}"#).unwrap();
        let mock = mock_provider(Some(script), matcher());
        let r = |s: &str| request(s);
        let text = |resp: CompletionResponse| resp.message.text();
        assert_eq!(
            text(mock.complete(r(&planner.system_prompt)).await.unwrap()),
            "routed"
        );
        assert_eq!(
            text(mock.complete(r(&planner.system_prompt)).await.unwrap()),
            "step"
        );
        let canned = text(mock.complete(r(&planner.system_prompt)).await.unwrap());
        assert!(canned.contains("\"phases\""));
    }

    #[tokio::test]
    async fn no_script_answers_with_canned_responses() {
        assert!(Script::default().fallback);
        let mock = mock_provider(None, matcher());
        let assessor =
            vibe_agents::builtin_agent(&vibe_core::AgentRole::ComplexityAssessor).unwrap();
        let text = mock
            .complete(request(&assessor.system_prompt))
            .await
            .unwrap()
            .message
            .text();
        assert!(text.contains("\"complexity\""), "{text}");
    }

    #[tokio::test]
    async fn exhausted_without_fallback_fails() {
        let script = Script::from_json(r#"{"fallback": false}"#).unwrap();
        let mock = mock_provider(Some(script), matcher());
        assert!(mock.complete(request("x")).await.is_err());
    }

    #[test]
    fn canned_answers_parse_as_json() {
        for role in [
            "complexity_assessor",
            "spec_gatherer",
            "planner",
            "coder",
            "qa_reviewer",
            "qa_fixer",
        ] {
            let text = canned_response(role).message.text();
            assert!(vibe_agents::extract_json(&text).is_some(), "{role}: {text}");
        }
    }
}
