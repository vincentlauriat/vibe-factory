//! `vibe plugins list|check`

use std::path::Path;

use anyhow::Result;
use serde_json::json;
use vibe_core::config::PluginConfig;
use vibe_plugins::{LoadedPlugin, PluginHost};

use crate::app::{load_config, plugin_configs};
use crate::cli::PluginsCommand;
use crate::util::{Table, Ui, style, truncate};

/// Run a `vibe plugins` subcommand.
pub async fn run(root: &Path, cmd: PluginsCommand, ui: Ui) -> Result<u8> {
    let config = load_config(root)?;
    let configs = plugin_configs(root, &config);
    let inline: Vec<&str> = config.plugins.iter().map(|p| p.name.as_str()).collect();
    match cmd {
        PluginsCommand::List => {
            if ui.json {
                let items: Vec<_> = configs
                    .iter()
                    .map(|c| {
                        json!({
                            "config": c,
                            "source": if inline.contains(&c.name.as_str()) { "config" } else { "manifest" },
                        })
                    })
                    .collect();
                ui.print_json(&json!(items));
                return Ok(0);
            }
            if configs.is_empty() {
                println!(
                    "No plugins. Declare one under [[plugins]] in .vibe/config.toml or add \
                     .vibe/plugins/<name>/vibe-plugin.toml"
                );
                return Ok(0);
            }
            let mut table = Table::new(["name", "source", "required", "capabilities", "command"]);
            for c in &configs {
                table.row([
                    c.name.clone(),
                    if inline.contains(&c.name.as_str()) {
                        "config".to_string()
                    } else {
                        "manifest".to_string()
                    },
                    if c.required { "yes" } else { "no" }.to_string(),
                    c.capabilities.join(", "),
                    truncate(&c.command.join(" "), 60),
                ]);
            }
            print!("{}", table.render());
            Ok(0)
        }
        PluginsCommand::Check => {
            let results = check_all(&configs).await;
            let failed = results.iter().filter(|r| r.1.is_err()).count();
            if ui.json {
                let items: Vec<_> = results
                    .iter()
                    .map(|(c, r)| match r {
                        Ok(p) => json!({
                            "name": c.name, "ok": true, "version": p.version,
                            "capabilities": p.capabilities, "tools": p.tool_names,
                        }),
                        Err(e) => json!({"name": c.name, "ok": false, "error": e}),
                    })
                    .collect();
                ui.print_json(&json!(items));
            } else {
                if results.is_empty() {
                    println!("No plugins to check.");
                }
                for (c, r) in &results {
                    match r {
                        Ok(p) => {
                            println!(
                                "{} {} {}",
                                style::ok().apply_to("✓"),
                                style::bold().apply_to(&c.name),
                                style::dim().apply_to(format!("v{}", p.version))
                            );
                            println!("    capabilities  {}", capabilities(p));
                            if !p.tool_names.is_empty() {
                                println!("    tools         {}", p.tool_names.join(", "));
                            }
                        }
                        Err(e) => println!(
                            "{} {}: {e}",
                            style::err().apply_to("✗"),
                            style::bold().apply_to(&c.name)
                        ),
                    }
                }
            }
            Ok(u8::from(failed > 0))
        }
    }
}

fn capabilities(p: &LoadedPlugin) -> String {
    let mut caps = Vec::new();
    if p.capabilities.tools {
        caps.push("tools");
    }
    if p.capabilities.agents {
        caps.push("agents");
    }
    if p.capabilities.hooks {
        caps.push("hooks");
    }
    if caps.is_empty() {
        "none".into()
    } else {
        caps.join(", ")
    }
}

/// Start each plugin on its own (so that every failure is reported), record
/// what it offers, then stop it.
pub async fn check_all(
    configs: &[PluginConfig],
) -> Vec<(PluginConfig, Result<LoadedPlugin, String>)> {
    let mut out = Vec::new();
    for config in configs {
        let mut strict = config.clone();
        strict.required = true;
        let result = match PluginHost::load(std::slice::from_ref(&strict)).await {
            Ok(mut host) => {
                let loaded = host.plugins().first().cloned();
                host.shutdown_all().await;
                loaded.ok_or_else(|| "plugin did not start".to_string())
            }
            Err(e) => Err(e.to_string()),
        };
        out.push((config.clone(), result));
    }
    out
}
