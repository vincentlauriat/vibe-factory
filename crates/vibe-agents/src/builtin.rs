//! Built-in agent definitions and user overrides loaded from TOML.

use std::path::{Path, PathBuf};

use vibe_core::agent::ThinkingLevel;
use vibe_core::{
    AgentRole, AgentSpec, Error, ModelRef, ModelSelection, Registry, Result, ToolSelection,
};

use crate::prompts::{builtin_prompt, strip_doc_comment};

/// Static settings of one built-in agent.
struct BuiltinDef {
    role: AgentRole,
    description: &'static str,
    tools: ToolSelection,
    thinking: ThinkingLevel,
    structured: bool,
    max_steps: u32,
    max_tokens: u32,
}

fn definitions() -> Vec<BuiltinDef> {
    use AgentRole as R;
    use ThinkingLevel as T;
    use ToolSelection as S;
    let def = |role, description, tools, thinking, structured, max_steps, max_tokens| BuiltinDef {
        role,
        description,
        tools,
        thinking,
        structured,
        max_steps,
        max_tokens,
    };
    vec![
        def(
            R::ComplexityAssessor,
            "Classifies the task complexity to pick a pipeline profile.",
            S::ReadOnly,
            T::Low,
            true,
            30,
            8_192,
        ),
        def(
            R::SpecGatherer,
            "Explores the codebase and extracts testable requirements.",
            S::ReadOnly,
            T::Medium,
            true,
            100,
            16_384,
        ),
        def(
            R::SpecResearcher,
            "Validates external libraries and APIs the requirements rely on.",
            S::ReadOnly,
            T::Medium,
            false,
            100,
            16_384,
        ),
        def(
            R::SpecWriter,
            "Writes the final specification (markdown file and JSON).",
            S::ReadWrite,
            T::High,
            true,
            100,
            32_000,
        ),
        def(
            R::SpecCritic,
            "Critiques the specification and returns a corrected version.",
            S::ReadOnly,
            T::High,
            true,
            60,
            32_000,
        ),
        def(
            R::Planner,
            "Breaks the specification into ordered, verifiable subtasks.",
            S::ReadOnly,
            T::High,
            true,
            100,
            32_000,
        ),
        def(
            R::Coder,
            "Implements and verifies one subtask with a fresh context.",
            S::All,
            T::Low,
            false,
            300,
            16_384,
        ),
        def(
            R::CoderRecovery,
            "Diagnoses a repeatedly failing subtask and completes it differently.",
            S::All,
            T::Medium,
            false,
            300,
            16_384,
        ),
        def(
            R::QaReviewer,
            "Verifies every acceptance criterion and issues a QA verdict.",
            S::All,
            T::High,
            true,
            200,
            32_000,
        ),
        def(
            R::QaFixer,
            "Fixes the issues listed in a QA report.",
            S::All,
            T::Medium,
            false,
            200,
            16_384,
        ),
        def(
            R::MergeResolver,
            "Resolves merge conflicts in one file.",
            S::None,
            T::Low,
            false,
            3,
            32_000,
        ),
        def(
            R::CommitMessage,
            "Writes a conventional commit message for staged changes.",
            S::None,
            T::Low,
            false,
            2,
            4_096,
        ),
    ]
}

/// Every built-in agent, in pipeline order.
#[must_use]
pub fn builtin_agents() -> Vec<AgentSpec> {
    definitions()
        .into_iter()
        .map(|d| {
            let prompt = builtin_prompt(&d.role).map_or("", strip_doc_comment);
            let mut spec = AgentSpec::new(d.role, prompt)
                .with_description(d.description)
                .with_tools(d.tools)
                .with_thinking(d.thinking)
                .with_max_steps(d.max_steps);
            spec.structured_output = d.structured;
            spec.max_tokens = d.max_tokens;
            spec
        })
        .collect()
}

/// The built-in agent for `role`, if there is one.
#[must_use]
pub fn builtin_agent(role: &AgentRole) -> Option<AgentSpec> {
    builtin_agents().into_iter().find(|s| &s.role == role)
}

/// Register every built-in agent in `registry` (replacing existing specs with
/// the same role).
pub fn register_builtin_agents(registry: &mut Registry) {
    for spec in builtin_agents() {
        registry.add_agent(spec);
    }
}

/// Directory holding a project's agent overrides: `.vibe/agents` under the
/// project root.
#[must_use]
pub fn project_agents_dir(project_root: impl AsRef<Path>) -> PathBuf {
    project_root.as_ref().join(".vibe").join("agents")
}

/// `tools` field of an override: a selection keyword, a table, or a plain
/// list of tool names.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ToolsField {
    List(Vec<String>),
    Selection(ToolSelection),
}

/// `model` field of an override: `"phase"`, `"provider/model"`, or a table.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ModelField {
    Text(String),
    Selection(ModelSelection),
}

/// One `.toml` override file.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentOverride {
    role: String,
    description: Option<String>,
    system_prompt: Option<String>,
    system_prompt_file: Option<PathBuf>,
    tools: Option<ToolsField>,
    model: Option<ModelField>,
    thinking: Option<ThinkingLevel>,
    max_steps: Option<u32>,
    structured_output: Option<bool>,
    max_tokens: Option<u32>,
}

/// Load agent definitions from every `*.toml` file of `dir` (usually
/// [`project_agents_dir`]), in file name order.
///
/// A file whose `role` matches a built-in agent overrides only the fields it
/// sets; any other role defines a new agent and must provide a prompt. A
/// missing directory yields an empty list. See the crate documentation for
/// the file format.
pub fn load_agent_overrides(dir: impl AsRef<Path>) -> Result<Vec<AgentSpec>> {
    load_agent_overrides_with_base(dir, &builtin_agents())
}

/// Like [`load_agent_overrides`], but overrides apply on top of `base`
/// (for example the agents already present in a [`Registry`]).
pub fn load_agent_overrides_with_base(
    dir: impl AsRef<Path>,
    base: &[AgentSpec],
) -> Result<Vec<AgentSpec>> {
    let dir = dir.as_ref();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut out: Vec<AgentSpec> = Vec::new();
    for file in files {
        let spec = load_one(&file, base, &out)
            .map_err(|e| Error::config(format!("{}: {}", file.display(), e.message)))?;
        out.retain(|s| s.role != spec.role);
        out.push(spec);
    }
    Ok(out)
}

/// Apply the overrides of `dir` to `registry`, on top of the agents it
/// already contains. Returns the number of agents added or replaced.
pub fn apply_agent_overrides(registry: &mut Registry, dir: impl AsRef<Path>) -> Result<usize> {
    let base: Vec<AgentSpec> = registry.agents.values().cloned().collect();
    let specs = load_agent_overrides_with_base(dir, &base)?;
    let n = specs.len();
    for spec in specs {
        registry.add_agent(spec);
    }
    Ok(n)
}

fn load_one(file: &Path, base: &[AgentSpec], loaded: &[AgentSpec]) -> Result<AgentSpec> {
    let text = std::fs::read_to_string(file)?;
    let o: AgentOverride = toml::from_str(&text)?;
    let role = AgentRole::parse(o.role.trim());
    if o.role.trim().is_empty() {
        return Err(Error::config("`role` must not be empty"));
    }

    let prompt = match (o.system_prompt, o.system_prompt_file) {
        (Some(_), Some(_)) => {
            return Err(Error::config(
                "set either `system_prompt` or `system_prompt_file`, not both",
            ));
        }
        (Some(p), None) => Some(p),
        (None, Some(rel)) => {
            let path = if rel.is_absolute() {
                rel
            } else {
                file.parent().unwrap_or(Path::new(".")).join(rel)
            };
            Some(std::fs::read_to_string(&path).map_err(|e| {
                Error::config(format!("cannot read prompt file {}: {e}", path.display()))
            })?)
        }
        (None, None) => None,
    };

    let existing = loaded.iter().chain(base).find(|s| s.role == role).cloned();
    let mut spec = match (existing, prompt) {
        (Some(mut s), Some(p)) => {
            s.system_prompt = p;
            s
        }
        (Some(s), None) => s,
        (None, Some(p)) => AgentSpec::new(role, p),
        (None, None) => {
            return Err(Error::config(format!(
                "agent `{}` is not a built-in agent: `system_prompt` or `system_prompt_file` is required",
                o.role.trim()
            )));
        }
    };

    if let Some(d) = o.description {
        spec.description = d;
    }
    if let Some(t) = o.tools {
        spec.tools = match t {
            ToolsField::List(names) => ToolSelection::Named(names),
            ToolsField::Selection(s) => s,
        };
    }
    if let Some(m) = o.model {
        spec.model = match m {
            ModelField::Selection(s) => s,
            ModelField::Text(t) if t.trim() == "phase" => ModelSelection::Phase,
            ModelField::Text(t) => match t.trim().split_once('/') {
                Some((p, m)) if !p.is_empty() && !m.is_empty() => {
                    ModelSelection::Fixed(ModelRef::new(p, m))
                }
                _ => {
                    return Err(Error::config(format!(
                        "`model = \"{t}\"`: use \"phase\" or \"provider/model\""
                    )));
                }
            },
        };
    }
    if let Some(t) = o.thinking {
        spec.thinking = t;
    }
    if let Some(n) = o.max_steps {
        spec.max_steps = n.max(1);
    }
    if let Some(b) = o.structured_output {
        spec.structured_output = b;
    }
    if let Some(n) = o.max_tokens {
        spec.max_tokens = n.max(1);
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn builtins_cover_every_role_with_expected_settings() {
        let agents = builtin_agents();
        assert_eq!(agents.len(), 12);
        let get = |r: AgentRole| agents.iter().find(|s| s.role == r).unwrap().clone();

        let ca = get(AgentRole::ComplexityAssessor);
        assert_eq!(ca.tools, ToolSelection::ReadOnly);
        assert_eq!(ca.thinking, ThinkingLevel::Low);
        assert!(ca.structured_output);

        let coder = get(AgentRole::Coder);
        assert_eq!(coder.tools, ToolSelection::All);
        assert_eq!(coder.thinking, ThinkingLevel::Low);
        assert!(!coder.structured_output);

        let writer = get(AgentRole::SpecWriter);
        assert_eq!(writer.tools, ToolSelection::ReadWrite);
        assert_eq!(writer.thinking, ThinkingLevel::High);

        let qa = get(AgentRole::QaReviewer);
        assert_eq!(qa.tools, ToolSelection::All);
        assert!(qa.structured_output);

        for r in [AgentRole::MergeResolver, AgentRole::CommitMessage] {
            let s = get(r);
            assert_eq!(s.tools, ToolSelection::None);
            assert_eq!(s.thinking, ThinkingLevel::Low);
        }
        for s in &agents {
            assert!(!s.system_prompt.contains("<!--"), "{}", s.role);
            assert!(!s.system_prompt.is_empty());
            assert!(!s.description.is_empty());
            // Output budget must leave room for the thinking budget.
            if let Some(b) = s.thinking.budget() {
                assert!(s.max_tokens > b, "{}", s.role);
            }
        }
    }

    #[test]
    fn register_in_registry() {
        let mut reg = Registry::new();
        register_builtin_agents(&mut reg);
        assert_eq!(reg.agents.len(), 12);
        assert!(reg.agent(&AgentRole::Planner).is_some());
        assert!(builtin_agent(&AgentRole::QaFixer).is_some());
        assert!(builtin_agent(&AgentRole::Custom("x".into())).is_none());
    }

    #[test]
    fn missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_agent_overrides(dir.path().join("nope"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn partial_override_of_builtin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("coder.toml"),
            "role = \"coder\"\nthinking = \"high\"\nmax_steps = 50\nmodel = \"openai/gpt-5\"\n",
        )
        .unwrap();
        let specs = load_agent_overrides(dir.path()).unwrap();
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.role, AgentRole::Coder);
        assert_eq!(s.thinking, ThinkingLevel::High);
        assert_eq!(s.max_steps, 50);
        assert_eq!(
            s.model,
            ModelSelection::Fixed(ModelRef::new("openai", "gpt-5"))
        );
        // Untouched fields keep the built-in values.
        assert_eq!(s.tools, ToolSelection::All);
        assert_eq!(
            s.system_prompt,
            builtin_agent(&AgentRole::Coder).unwrap().system_prompt
        );
    }

    #[test]
    fn new_agent_with_prompt_file_and_tool_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("doc.md"), "Write docs for {{task_title}}.").unwrap();
        std::fs::write(
            dir.path().join("doc_writer.toml"),
            r#"
role = "doc_writer"
description = "Writes documentation"
system_prompt_file = "doc.md"
tools = ["read_file", "write_file"]
structured_output = true
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("reviewer.toml"),
            "role = \"linter\"\nsystem_prompt = \"Lint.\"\ntools = \"read_only\"\nmodel = \"phase\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "not toml").unwrap();
        let specs = load_agent_overrides(dir.path()).unwrap();
        assert_eq!(specs.len(), 2);
        let doc = &specs[0];
        assert_eq!(doc.role, AgentRole::Custom("doc_writer".into()));
        assert_eq!(doc.system_prompt, "Write docs for {{task_title}}.");
        assert_eq!(
            doc.tools,
            ToolSelection::Named(vec!["read_file".into(), "write_file".into()])
        );
        assert!(doc.structured_output);
        assert_eq!(specs[1].tools, ToolSelection::ReadOnly);
        assert_eq!(specs[1].model, ModelSelection::Phase);
    }

    #[test]
    fn named_tools_table_form() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.toml"),
            "role = \"planner\"\ntools = { named = [\"grep\"] }\n",
        )
        .unwrap();
        let specs = load_agent_overrides(dir.path()).unwrap();
        assert_eq!(specs[0].tools, ToolSelection::Named(vec!["grep".into()]));
    }

    #[test]
    fn invalid_overrides_are_reported() {
        let cases = [
            "role = \"brand_new\"\n",
            "role = \"coder\"\nsystem_prompt = \"a\"\nsystem_prompt_file = \"b\"\n",
            "role = \"coder\"\nunknown_field = 1\n",
            "role = \"coder\"\nmodel = \"bare-model\"\n",
            "role = \"coder\"\nsystem_prompt_file = \"missing.md\"\n",
            "role = \"\"\nsystem_prompt = \"x\"\n",
            "not toml at all = = =",
        ];
        for case in cases {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("x.toml"), case).unwrap();
            let err = load_agent_overrides(dir.path()).unwrap_err();
            assert_eq!(err.kind, vibe_core::ErrorKind::Config, "{case}");
            assert!(err.message.contains("x.toml"), "{case}: {}", err.message);
        }
    }

    #[test]
    fn apply_overrides_on_registry_agents() {
        let mut reg = Registry::new();
        register_builtin_agents(&mut reg);
        let root = tempfile::tempdir().unwrap();
        let dir = project_agents_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("qa.toml"),
            "role = \"qa_reviewer\"\nmax_steps = 7\n",
        )
        .unwrap();
        assert_eq!(apply_agent_overrides(&mut reg, &dir).unwrap(), 1);
        assert_eq!(reg.agent(&AgentRole::QaReviewer).unwrap().max_steps, 7);
        assert_eq!(reg.agents.len(), 12);
    }
}
