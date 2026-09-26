//! Plugin manifests (`vibe-plugin.toml`).
//!
//! ```toml
//! name = "echo"
//! version = "0.1.0"
//! description = "Echoes its input"
//! command = ["./bin/echo-plugin", "--quiet"]
//! required = false
//! capabilities = ["tools", "agents"]   # or a table: [capabilities] tools = true
//!
//! [env]
//! ECHO_PREFIX = ">"
//! ```
//!
//! The plugin process runs in the directory holding the manifest, so
//! relative arguments are relative to it. A program written as a relative
//! path (`./bin/echo-plugin`) is resolved against that directory too, while
//! a bare program name (`python3`) is looked up on `PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use vibe_core::config::PluginConfig;
use vibe_core::{Error, Result};

use crate::protocol::Capabilities;

/// File name of a plugin manifest.
pub const MANIFEST_FILE: &str = "vibe-plugin.toml";

/// Parsed `vibe-plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin name.
    pub name: String,
    /// Semantic version.
    #[serde(default)]
    pub version: String,
    /// One-line description.
    #[serde(default)]
    pub description: String,
    /// Program and arguments speaking the protocol over stdio.
    pub command: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Declared capabilities, as a list of names or a table of booleans
    /// (informational; the handshake is authoritative).
    #[serde(default, deserialize_with = "capabilities_list_or_table")]
    pub capabilities: Capabilities,
    /// Whether a failure to start is fatal.
    #[serde(default)]
    pub required: bool,
    /// Directory the manifest was loaded from; the plugin runs there.
    #[serde(skip)]
    pub dir: Option<PathBuf>,
}

fn capabilities_list_or_table<'de, D: Deserializer<'de>>(d: D) -> Result<Capabilities, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Form {
        List(Vec<String>),
        Table(Capabilities),
    }
    Ok(match Form::deserialize(d)? {
        Form::List(names) => Capabilities::from_names(names),
        Form::Table(caps) => caps,
    })
}

impl PluginManifest {
    /// Parse manifest text found in directory `dir`.
    pub fn from_toml(text: &str, dir: &Path) -> Result<Self> {
        let mut manifest: Self = toml::from_str(text)?;
        manifest.validate()?;
        manifest.dir = Some(dir.to_path_buf());
        Ok(manifest)
    }

    /// Load a manifest file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml(&text, dir)
            .map_err(|e| Error::config(format!("{}: {}", path.display(), e.message)))
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::config("plugin manifest: `name` must not be empty"));
        }
        if self.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(Error::config(format!(
                "plugin manifest `{}`: `command` must name a program",
                self.name
            )));
        }
        Ok(())
    }

    /// Conversion into the configuration consumed by the plugin host: the
    /// working directory is the manifest's directory.
    #[must_use]
    pub fn to_config(&self) -> PluginConfig {
        PluginConfig {
            name: self.name.clone(),
            command: self.command.clone(),
            env: self.env.clone(),
            cwd: self.dir.clone(),
            capabilities: self.capabilities.names(),
            required: self.required,
        }
    }
}

impl From<PluginManifest> for PluginConfig {
    fn from(m: PluginManifest) -> Self {
        m.to_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TEXT: &str = r#"
name = "echo"
version = "0.2.0"
description = "Echo"
command = ["./bin/echo", "--flag", "server.py", "python3"]
required = true

[env]
A = "1"

[capabilities]
tools = true
hooks = true
"#;

    #[test]
    fn parses_and_keeps_command_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::from_toml(TEXT, dir.path()).unwrap();
        assert_eq!(m.name, "echo");
        assert_eq!(m.version, "0.2.0");
        assert!(m.required);
        assert!(m.capabilities.tools && m.capabilities.hooks && !m.capabilities.agents);
        assert_eq!(m.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(
            m.command,
            vec!["./bin/echo", "--flag", "server.py", "python3"]
        );
        assert_eq!(m.dir.as_deref(), Some(dir.path()));
    }

    #[test]
    fn capabilities_as_list() {
        let m = PluginManifest::from_toml(
            "name = \"x\"\ncommand = [\"x\"]\ncapabilities = [\"agents\"]\n",
            Path::new("."),
        )
        .unwrap();
        assert!(m.capabilities.agents && !m.capabilities.tools);
    }

    #[test]
    fn converts_to_plugin_config() {
        let dir = tempfile::tempdir().unwrap();
        let m = PluginManifest::from_toml(TEXT, dir.path()).unwrap();
        let cfg = m.to_config();
        assert_eq!(cfg.name, "echo");
        assert!(cfg.required);
        assert_eq!(cfg.command.len(), 4);
        assert_eq!(cfg.cwd.as_deref(), Some(dir.path()));
        assert_eq!(cfg.capabilities, vec!["tools", "hooks"]);
        assert_eq!(PluginConfig::from(m), cfg);
    }

    #[test]
    fn rejects_invalid_manifests() {
        let dir = Path::new(".");
        assert!(PluginManifest::from_toml("name = \"\"\ncommand = [\"x\"]", dir).is_err());
        assert!(PluginManifest::from_toml("name = \"a\"\ncommand = []", dir).is_err());
        assert!(PluginManifest::from_toml("name = \"a\"", dir).is_err());
        let err = PluginManifest::from_toml("not toml", dir).unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::Config);
    }

    #[test]
    fn load_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        std::fs::write(&path, "name = 1").unwrap();
        let err = PluginManifest::load(&path).unwrap_err();
        assert!(err.message.contains(MANIFEST_FILE));
    }
}
