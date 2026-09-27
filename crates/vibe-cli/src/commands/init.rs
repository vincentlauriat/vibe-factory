//! `vibe init`

use std::path::Path;

use anyhow::{Context, Result};
use vibe_core::VibeConfig;
use vibe_core::config::VIBE_DIR;

use crate::app::{config_path, is_git_repo};
use crate::util::{Ui, style};

/// Entries of `.vibe/.gitignore`: generated workspaces and tool outputs.
pub const VIBE_GITIGNORE: &str = "# tasks/ is committed on purpose: specs, plans and QA reports are project history.\n\
worktrees/\n\
tool-output/\n";

const HEADER: &str = "# Vibe Factory configuration.\n\
# Edit by hand or with `vibe config set <key> <value>`; see `vibe config show --default`.\n\n";

const PLUGINS_EXAMPLE: &str = "\n# Plugins (tools, agents, hooks) speaking the stdio protocol:\n\
# [[plugins]]\n\
# name = \"my-plugin\"\n\
# command = [\"./path/to/plugin\"]\n";

/// Text of a fresh `.vibe/config.toml`: the defaults, without the empty
/// `plugins = []` (which would forbid appending `[[plugins]]` tables), plus
/// a commented plugin example.
pub fn initial_config() -> Result<String> {
    let text = VibeConfig::default().to_toml()?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    if doc
        .get("plugins")
        .and_then(toml_edit::Item::as_array)
        .is_some_and(toml_edit::Array::is_empty)
    {
        doc.remove("plugins");
    }
    Ok(format!(
        "{HEADER}{}{PLUGINS_EXAMPLE}",
        doc.to_string().trim_end()
    ))
}

/// Write `.vibe/config.toml` (and `.vibe/.gitignore` in a git repository).
pub async fn run(root: &Path, force: bool, ui: Ui) -> Result<u8> {
    let path = config_path(root);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use `vibe init --force` to overwrite it)",
            path.display()
        );
    }
    let dir = root.join(VIBE_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(&path, initial_config()?)
        .with_context(|| format!("cannot write {}", path.display()))?;

    let git = is_git_repo(root).await;
    let mut gitignore = None;
    if git {
        let p = dir.join(".gitignore");
        let existing = std::fs::read_to_string(&p).unwrap_or_default();
        let mut content = existing.clone();
        for line in VIBE_GITIGNORE.lines() {
            if !existing.lines().any(|l| l.trim() == line) {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(line);
                content.push('\n');
            }
        }
        if content != existing {
            std::fs::write(&p, content).with_context(|| format!("cannot write {}", p.display()))?;
        }
        gitignore = Some(p);
    }

    if ui.json {
        ui.print_json(&serde_json::json!({
            "config": path,
            "gitignore": gitignore,
            "git": git,
        }));
        return Ok(0);
    }
    println!(
        "{} wrote {}",
        style::ok().apply_to("✓"),
        style::bold().apply_to(path.display())
    );
    if let Some(p) = gitignore {
        println!("{} updated {}", style::ok().apply_to("✓"), p.display());
    } else {
        println!(
            "{} not a git repository: run `git init` (worktree isolation needs git) or set \
             `pipeline.workspace = \"in_place\"`",
            style::warn().apply_to("!")
        );
    }
    println!("\nNext steps:");
    println!("  export ANTHROPIC_API_KEY=…            # or OPENAI_API_KEY, or run Ollama");
    println!("  vibe doctor                           # check the setup");
    println!("  vibe task add \"Describe your change\"");
    println!("  vibe run 1                            # or: vibe run 1 --provider mock --dry-run");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_config_is_the_default_and_accepts_plugin_tables() {
        let text = initial_config().unwrap();
        assert!(!text.contains("plugins = []"));
        assert_eq!(VibeConfig::from_toml(&text).unwrap(), VibeConfig::default());
        let extended = format!("{text}\n[[plugins]]\nname = \"p\"\ncommand = [\"x\"]\n");
        assert_eq!(VibeConfig::from_toml(&extended).unwrap().plugins.len(), 1);
    }
}
