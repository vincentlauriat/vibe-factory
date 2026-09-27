//! `vibe config show|path|set`

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};
use vibe_core::VibeConfig;

use crate::app::{config_path, load_config};
use crate::cli::ConfigCommand;
use crate::util::{Ui, style};

/// Run a `vibe config` subcommand.
pub fn run(root: &Path, cmd: ConfigCommand, ui: Ui) -> Result<u8> {
    match cmd {
        ConfigCommand::Show { default } => {
            let config = if default {
                VibeConfig::default()
            } else {
                load_config(root)?
            };
            if ui.json {
                ui.print_json(&serde_json::to_value(&config)?);
            } else {
                if !default && !config_path(root).exists() {
                    println!(
                        "# {} does not exist: built-in defaults",
                        config_path(root).display()
                    );
                }
                print!("{}", config.to_toml()?);
            }
            Ok(0)
        }
        ConfigCommand::Path => {
            let path = config_path(root);
            if ui.json {
                ui.print_json(&serde_json::json!({"path": path, "exists": path.exists()}));
            } else {
                println!("{}", path.display());
            }
            Ok(0)
        }
        ConfigCommand::Set { key, value } => {
            let path = config_path(root);
            let text = if path.exists() {
                std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?
            } else {
                super::init::initial_config()?
            };
            let updated = set_value(&text, &key, &value)?;
            VibeConfig::from_toml(&updated)
                .map_err(|e| anyhow!("`{key} = {value}` makes the configuration invalid: {e}"))?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &updated)
                .with_context(|| format!("cannot write {}", path.display()))?;
            if ui.json {
                ui.print_json(&serde_json::json!({"path": path, "key": key, "value": value}));
            } else {
                println!(
                    "{} {key} = {}",
                    style::ok().apply_to("✓"),
                    parse_value(&value)
                );
            }
            Ok(0)
        }
    }
}

/// Parse a command line value: a TOML literal (`true`, `3`, `1.5`,
/// `["a", "b"]`, `"quoted"`, `{ a = 1 }`) or, failing that, a plain string.
pub fn parse_value(raw: &str) -> Value {
    let doc = format!("v = {raw}");
    if let Ok(parsed) = doc.parse::<DocumentMut>()
        && let Some(v) = parsed.get("v").and_then(Item::as_value)
    {
        let mut v = v.clone();
        v.decor_mut().clear();
        return v;
    }
    Value::from(raw)
}

/// Set `key` (dotted) to `raw` in the TOML `text`, preserving the rest of
/// the document; intermediate tables are created as needed.
pub fn set_value(text: &str, key: &str, raw: &str) -> Result<String> {
    let mut doc: DocumentMut = text
        .parse()
        .context("the configuration is not valid TOML")?;
    let parts: Vec<&str> = key.split('.').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        bail!("invalid key `{key}`");
    }
    let (last, parents) = parts.split_last().ok_or_else(|| anyhow!("empty key"))?;
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    let mut walked = Vec::new();
    for part in parents {
        walked.push(*part);
        if current.get(part).is_none() {
            let mut t = Table::new();
            t.set_implicit(true);
            current.insert(part, Item::Table(t));
        }
        current = current
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("`{}` is not a table", walked.join(".")))?;
    }
    current.insert(last, Item::Value(parse_value(raw)));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_typed() {
        assert_eq!(parse_value("true").as_bool(), Some(true));
        assert_eq!(parse_value("3").as_integer(), Some(3));
        assert_eq!(parse_value("openai/gpt-5").as_str(), Some("openai/gpt-5"));
        assert_eq!(parse_value("\"x y\"").as_str(), Some("x y"));
        assert!(parse_value("[\"a\", \"b\"]").is_array());
    }

    #[test]
    fn set_preserves_comments_and_creates_tables() {
        let text = "# keep me\ndefault_provider = \"anthropic\"\n\n[pipeline]\nauto_merge = false # inline\n";
        let out = set_value(text, "pipeline.auto_merge", "true").unwrap();
        assert!(out.contains("# keep me"));
        assert!(out.contains("auto_merge = true"));
        let out = set_value(&out, "phases.plan.model", "openai/gpt-5").unwrap();
        assert!(out.contains("[phases.plan]"), "{out}");
        let cfg = VibeConfig::from_toml(&out).unwrap();
        assert!(cfg.pipeline.auto_merge);
        assert_eq!(cfg.phases.phases["plan"].model, "openai/gpt-5");
        assert!(set_value(&out, "default_provider.x", "1").is_err());
        assert!(set_value(&out, "a..b", "1").is_err());
    }
}
