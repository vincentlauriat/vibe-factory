//! `vibe agents list|show|export`

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use vibe_core::{AgentRole, AgentSpec, ModelSelection, Registry, ToolSelection};

use crate::app::agent_registry;
use crate::cli::AgentsCommand;
use crate::util::{Table, Ui, enum_name, style, truncate};

/// Run a `vibe agents` subcommand.
pub fn run(root: &Path, cmd: AgentsCommand, ui: Ui) -> Result<u8> {
    let registry = agent_registry(root)?;
    match cmd {
        AgentsCommand::List => list(&registry, ui),
        AgentsCommand::Show { role } => {
            let spec = find(&registry, &role)?;
            if ui.json {
                ui.print_json(&serde_json::to_value(spec)?);
            } else {
                println!("{}", spec.system_prompt.trim_end());
            }
            Ok(0)
        }
        AgentsCommand::Export { role, dir, force } => {
            let spec = find(&registry, &role)?;
            let dir = dir.unwrap_or_else(|| vibe_agents::project_agents_dir(root));
            let (toml_path, prompt_path) = export(spec, &dir, force)?;
            if ui.json {
                ui.print_json(&json!({"agent": toml_path, "prompt": prompt_path}));
            } else {
                println!(
                    "{} wrote {}",
                    style::ok().apply_to("✓"),
                    toml_path.display()
                );
                println!(
                    "{} wrote {}",
                    style::ok().apply_to("✓"),
                    prompt_path.display()
                );
                println!("\nEdit them; they are loaded on the next run.");
            }
            Ok(0)
        }
    }
}

fn find<'a>(registry: &'a Registry, role: &str) -> Result<&'a AgentSpec> {
    registry
        .agent(&AgentRole::parse(role.trim()))
        .ok_or_else(|| {
            anyhow!(
                "unknown agent `{role}` (known: {})",
                registry
                    .agents
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Where an agent definition comes from.
pub fn source(spec: &AgentSpec) -> &'static str {
    match vibe_agents::builtin_agent(&spec.role) {
        Some(builtin) if &builtin == spec => "builtin",
        Some(_) => "override",
        None => "custom",
    }
}

fn tools_text(sel: &ToolSelection) -> String {
    match sel {
        ToolSelection::None => "none".into(),
        ToolSelection::ReadOnly => "read_only".into(),
        ToolSelection::ReadWrite => "read_write".into(),
        ToolSelection::All => "all".into(),
        ToolSelection::Named(names) => names.join(", "),
    }
}

fn model_text(sel: &ModelSelection) -> String {
    match sel {
        ModelSelection::Phase => "phase".into(),
        ModelSelection::Fixed(m) => m.to_string(),
    }
}

fn list(registry: &Registry, ui: Ui) -> Result<u8> {
    if ui.json {
        let items: Vec<_> = registry
            .agents
            .values()
            .map(|s| {
                json!({
                    "role": s.role,
                    "description": s.description,
                    "tools": s.tools,
                    "thinking": s.thinking,
                    "model": model_text(&s.model),
                    "max_steps": s.max_steps,
                    "structured_output": s.structured_output,
                    "source": source(s),
                })
            })
            .collect();
        ui.print_json(&json!(items));
        return Ok(0);
    }
    // Built-in roles in pipeline order, then the others.
    let mut specs: Vec<&AgentSpec> = AgentRole::builtin()
        .filter_map(|r| registry.agent(&r))
        .collect();
    specs.extend(registry.agents.values().filter(|s| !s.role.is_builtin()));
    let mut table = Table::new([
        "role",
        "tools",
        "thinking",
        "model",
        "source",
        "description",
    ]);
    for s in specs {
        let src = source(s);
        let src = if src == "builtin" {
            style::dim().apply_to(src).to_string()
        } else {
            style::warn().apply_to(src).to_string()
        };
        table.row([
            s.role.name(),
            truncate(&tools_text(&s.tools), 30),
            enum_name(&s.thinking),
            model_text(&s.model),
            src,
            truncate(&s.description, 50),
        ]);
    }
    print!("{}", table.render());
    Ok(0)
}

/// Serialised override file, in the format `vibe_agents` loads.
#[derive(serde::Serialize)]
struct ExportedAgent {
    role: String,
    description: String,
    system_prompt_file: String,
    tools: toml::Value,
    model: String,
    thinking: vibe_core::ThinkingLevel,
    max_steps: u32,
    max_tokens: u32,
    structured_output: bool,
}

/// Write `<dir>/<role>.toml` and `<dir>/<role>.md` for `spec`.
pub fn export(spec: &AgentSpec, dir: &Path, force: bool) -> Result<(PathBuf, PathBuf)> {
    let name = spec.role.name();
    let toml_path = dir.join(format!("{name}.toml"));
    let prompt_path = dir.join(format!("{name}.md"));
    if !force {
        for p in [&toml_path, &prompt_path] {
            if p.exists() {
                bail!("{} already exists (use --force to overwrite)", p.display());
            }
        }
    }
    let tools = match &spec.tools {
        ToolSelection::Named(names) => {
            toml::Value::Array(names.iter().cloned().map(toml::Value::String).collect())
        }
        other => toml::Value::String(tools_text(other)),
    };
    let exported = ExportedAgent {
        role: name.clone(),
        description: spec.description.clone(),
        system_prompt_file: format!("{name}.md"),
        tools,
        model: model_text(&spec.model),
        thinking: spec.thinking,
        max_steps: spec.max_steps,
        max_tokens: spec.max_tokens,
        structured_output: spec.structured_output,
    };
    let body = toml::to_string_pretty(&exported).context("cannot serialise the agent")?;
    let header = format!(
        "# Override of the `{name}` agent. Fields you delete keep their built-in value.\n\
         # tools: \"none\" | \"read_only\" | \"read_write\" | \"all\" | [\"tool\", …]\n\
         # model: \"phase\" (the phase model) or \"provider/model\"\n\n"
    );
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(&toml_path, format!("{header}{body}"))
        .with_context(|| format!("cannot write {}", toml_path.display()))?;
    std::fs::write(&prompt_path, &spec.system_prompt)
        .with_context(|| format!("cannot write {}", prompt_path.display()))?;
    Ok((toml_path, prompt_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_round_trips_through_the_override_loader() {
        let dir = tempfile::tempdir().unwrap();
        for spec in vibe_agents::builtin_agents() {
            export(&spec, dir.path(), false).unwrap();
        }
        let mut registry = Registry::new();
        vibe_agents::register_builtin_agents(&mut registry);
        let n = vibe_agents::apply_agent_overrides(&mut registry, dir.path()).unwrap();
        assert_eq!(n, vibe_agents::builtin_agents().len());
        for spec in vibe_agents::builtin_agents() {
            assert_eq!(registry.agent(&spec.role), Some(&spec), "{}", spec.role);
            assert_eq!(source(&spec), "builtin");
        }
        let planner = vibe_agents::builtin_agent(&AgentRole::Planner).unwrap();
        assert!(
            export(&planner, dir.path(), false).is_err(),
            "refuses to overwrite"
        );
        export(&planner, dir.path(), true).unwrap();
    }

    #[test]
    fn named_tools_and_fixed_models_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = vibe_agents::builtin_agent(&AgentRole::Coder).unwrap();
        spec.tools = ToolSelection::Named(vec!["read_file".into(), "bash".into()]);
        spec.model = ModelSelection::Fixed(vibe_core::ModelRef::new("openai", "gpt-5"));
        export(&spec, dir.path(), false).unwrap();
        let loaded = vibe_agents::load_agent_overrides(dir.path()).unwrap();
        assert_eq!(loaded, vec![spec.clone()]);
        assert_eq!(source(&spec), "override");
    }
}
