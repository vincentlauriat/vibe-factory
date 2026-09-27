//! `vibe doctor`

use std::path::Path;

use anyhow::Result;
use serde_json::json;
use vibe_core::VibeConfig;
use vibe_core::config::VIBE_DIR;

use super::plugins::check_all;
use crate::app::{MOCK_PROVIDER, config_path, is_git_repo, plugin_configs, referenced_providers};
use crate::util::{Ui, style};

/// Result of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Fine.
    Ok,
    /// Worth knowing, not blocking.
    Warn,
    /// Blocking.
    Fail,
}

#[derive(Debug, serde::Serialize)]
struct Check {
    level: Level,
    name: &'static str,
    message: String,
}

#[derive(Default)]
struct Report(Vec<Check>);

impl Report {
    fn add(&mut self, level: Level, name: &'static str, message: impl Into<String>) {
        self.0.push(Check {
            level,
            name,
            message: message.into(),
        });
    }
}

/// Kinds of providers that never need an API key.
const KEYLESS_KINDS: &[&str] = &["mock", "ollama"];

/// Check the environment and the configuration; exit code 1 on any failure.
pub async fn run(root: &Path, ui: Ui) -> Result<u8> {
    let mut r = Report::default();

    // git
    match tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => r.add(
            Level::Ok,
            "git",
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        Ok(out) => r.add(
            Level::Fail,
            "git",
            format!(
                "`git --version` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ),
        Err(e) => r.add(Level::Fail, "git", format!("git not found: {e}")),
    }

    // configuration
    let path = config_path(root);
    let config = if path.exists() {
        match VibeConfig::load(root) {
            Ok(c) => {
                r.add(Level::Ok, "config", format!("{} parses", path.display()));
                Some(c)
            }
            Err(e) => {
                r.add(Level::Fail, "config", format!("{}: {e}", path.display()));
                None
            }
        }
    } else {
        r.add(
            Level::Warn,
            "config",
            format!(
                "{} missing, using defaults (run `vibe init`)",
                path.display()
            ),
        );
        Some(VibeConfig::default())
    };

    // repository
    let git_repo = is_git_repo(root).await;
    let needs_git = config
        .as_ref()
        .is_none_or(|c| crate::app::uses_worktrees(&c.pipeline.workspace));
    match (git_repo, needs_git) {
        (true, _) => r.add(
            Level::Ok,
            "repository",
            format!("{} is a git repository", root.display()),
        ),
        (false, true) => r.add(
            Level::Fail,
            "repository",
            format!(
                "{} is not a git repository (needed by the git_worktree and container workspaces)",
                root.display()
            ),
        ),
        (false, false) => r.add(Level::Warn, "repository", "not a git repository"),
    }

    if let Some(config) = &config {
        check_providers(config, &mut r);

        let configs = plugin_configs(root, config);
        let results = check_all(&configs).await;
        for (c, res) in &results {
            match res {
                Ok(p) => r.add(
                    Level::Ok,
                    "plugin",
                    format!(
                        "{} v{} started ({} tool(s))",
                        c.name,
                        p.version,
                        p.tool_names.len()
                    ),
                ),
                Err(e) => r.add(
                    if c.required { Level::Fail } else { Level::Warn },
                    "plugin",
                    format!("{}: {e}", c.name),
                ),
            }
        }

        let ws = config.pipeline.workspace.as_str();
        if matches!(ws, "git_worktree" | "in_place") {
            r.add(Level::Ok, "workspace", format!("`{ws}`"));
        } else if ws == vibe_workspace::container::PROVIDER_NAME {
            check_container(root, config, &mut r).await;
        } else if results.iter().any(|(_, res)| res.is_ok()) {
            r.add(
                Level::Warn,
                "workspace",
                format!("`{ws}` is not built in; it must be provided by a plugin"),
            );
        } else {
            r.add(
                Level::Fail,
                "workspace",
                format!(
                    "unknown workspace provider `{ws}` (built in: git_worktree, in_place, container)"
                ),
            );
        }
    }

    // .vibe writable
    let vibe_dir = root.join(VIBE_DIR);
    let probe_dir = if vibe_dir.is_dir() {
        vibe_dir.clone()
    } else {
        root.to_path_buf()
    };
    let probe = probe_dir.join(format!(".vibe-doctor-{}", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            r.add(
                Level::Ok,
                "storage",
                format!("{} is writable", probe_dir.display()),
            );
        }
        Err(e) => r.add(
            Level::Fail,
            "storage",
            format!("{} is not writable: {e}", probe_dir.display()),
        ),
    }

    let failed = r.0.iter().any(|c| c.level == Level::Fail);
    if ui.json {
        ui.print_json(&json!({"ok": !failed, "checks": r.0}));
    } else {
        for c in &r.0 {
            let mark = match c.level {
                Level::Ok => style::ok().apply_to("✓"),
                Level::Warn => style::warn().apply_to("!"),
                Level::Fail => style::err().apply_to("✗"),
            };
            println!("{mark} {:<11} {}", c.name, c.message);
        }
        if failed {
            println!("\n{}", style::err().apply_to("Some checks failed."));
        } else {
            println!("\n{}", style::ok().apply_to("All checks passed."));
        }
    }
    Ok(u8::from(failed))
}

fn check_providers(config: &VibeConfig, r: &mut Report) {
    if let Err(e) = vibe_providers::ProviderRegistry::from_config(config) {
        r.add(Level::Fail, "providers", e.to_string());
        return;
    }
    let used = referenced_providers(config);
    for name in &used {
        if name == MOCK_PROVIDER && config.provider(name).is_none() {
            r.add(Level::Ok, "provider", "mock (built in, no key needed)");
        } else if config.provider(name).is_none() {
            r.add(
                Level::Fail,
                "provider",
                format!("`{name}` is used by the configuration but not declared under [providers]"),
            );
        }
    }
    for (name, p) in &config.providers {
        let in_use = used.contains(name);
        let needs_key = !KEYLESS_KINDS.contains(&p.kind.trim().to_ascii_lowercase().as_str())
            && (p.api_key.is_some() || p.api_key_env.is_some());
        let usage = if in_use { "" } else { " (unused)" };
        if !needs_key {
            r.add(
                Level::Ok,
                "provider",
                format!("{name}: no key needed{usage}"),
            );
        } else if p.resolve_api_key().is_some() {
            r.add(
                Level::Ok,
                "provider",
                format!("{name}: API key found{usage}"),
            );
        } else {
            let var = p.api_key_env.clone().unwrap_or_else(|| "api_key".into());
            r.add(
                if in_use { Level::Fail } else { Level::Warn },
                "provider",
                format!("{name}: no API key (set {var}){usage}"),
            );
        }
    }
}

/// Check the `container` workspace: valid settings and a reachable runtime.
async fn check_container(root: &Path, config: &VibeConfig, r: &mut Report) {
    let settings = match crate::app::container_settings(root, config) {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => {
            r.add(Level::Fail, "workspace", format!("`container`: {e:#}"));
            return;
        }
    };
    if vibe_workspace::container::runtime_available(&settings.runtime).await {
        r.add(
            Level::Ok,
            "workspace",
            format!(
                "`container`: image `{}`, network `{}`, `{} info` answers",
                settings.image,
                settings.effective_network(config.security.allow_network),
                settings.runtime
            ),
        );
    } else {
        r.add(
            Level::Fail,
            "workspace",
            format!(
                "`container`: `{} info` failed; install the runtime or start its daemon",
                settings.runtime
            ),
        );
    }
}
