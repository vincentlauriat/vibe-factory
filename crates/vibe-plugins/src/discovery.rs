//! Finding plugins on disk.
//!
//! Plugins are looked up in two places, in this order:
//!
//! 1. `<project>/.vibe/plugins/*/vibe-plugin.toml`
//! 2. `<user config dir>/vibe/plugins/*/vibe-plugin.toml` (for example
//!    `~/.config/vibe/plugins` on Linux)
//!
//! When two manifests share a name, the first one found wins, so project
//! plugins shadow user plugins. Inline `[[plugins]]` declarations of the
//! project configuration shadow both (see [`merge_with_config`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vibe_core::VibeConfig;
use vibe_core::config::{PluginConfig, VIBE_DIR};

use crate::manifest::{MANIFEST_FILE, PluginManifest};

/// Name of the sub-directory holding plugins.
pub const PLUGINS_DIR: &str = "plugins";

/// `<project>/.vibe/plugins`.
#[must_use]
pub fn project_plugins_dir(project_root: &Path) -> PathBuf {
    project_root.join(VIBE_DIR).join(PLUGINS_DIR)
}

/// `<user config dir>/vibe/plugins`, when the platform has a config dir.
#[must_use]
pub fn user_plugins_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("vibe").join(PLUGINS_DIR))
}

/// Discover the project's and the user's plugins (project wins on name
/// clashes).
#[must_use]
pub fn discover(project_root: &Path) -> Vec<PluginManifest> {
    let mut dirs = vec![project_plugins_dir(project_root)];
    dirs.extend(user_plugins_dir());
    discover_in(&dirs)
}

/// Discover plugins in the given directories, earlier directories taking
/// precedence on name clashes.
#[must_use]
pub fn discover_in(dirs: &[PathBuf]) -> Vec<PluginManifest> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for manifest in scan_dir(dir) {
            if seen.insert(manifest.name.clone()) {
                out.push(manifest);
            } else {
                tracing::debug!(
                    "plugin `{}` in {} is shadowed by an earlier one",
                    manifest.name,
                    dir.display()
                );
            }
        }
    }
    out
}

/// Load every `<dir>/*/vibe-plugin.toml`, sorted by sub-directory name.
/// A missing directory yields nothing; invalid manifests are logged and
/// skipped.
#[must_use]
pub fn scan_dir(dir: &Path) -> Vec<PluginManifest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .map(|p| p.join(MANIFEST_FILE))
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| match PluginManifest::load(path) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("skipping invalid plugin manifest: {e}");
                None
            }
        })
        .collect()
}

/// Combine the inline declarations of `config` with discovered manifests.
/// Inline declarations come first and shadow manifests of the same name.
#[must_use]
pub fn merge_with_config(discovered: &[PluginManifest], config: &VibeConfig) -> Vec<PluginConfig> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for inline in &config.plugins {
        if seen.insert(inline.name.clone()) {
            out.push(inline.clone());
        }
    }
    for manifest in discovered {
        if seen.insert(manifest.name.clone()) {
            out.push(manifest.to_config());
        }
    }
    out
}

/// Everything to load for a project: [`discover`] merged with the inline
/// declarations of `config`.
#[must_use]
pub fn plugin_configs(project_root: &Path, config: &VibeConfig) -> Vec<PluginConfig> {
    merge_with_config(&discover(project_root), config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_manifest(root: &Path, dir: &str, name: &str, version: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(MANIFEST_FILE),
            format!("name = \"{name}\"\nversion = \"{version}\"\ncommand = [\"run-{name}\"]\n"),
        )
        .unwrap();
    }

    #[test]
    fn scans_sorted_and_skips_invalid() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), "b", "beta", "1");
        write_manifest(root.path(), "a", "alpha", "1");
        let bad = root.path().join("c");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join(MANIFEST_FILE), "garbage = ").unwrap();
        std::fs::create_dir_all(root.path().join("empty")).unwrap();
        std::fs::write(root.path().join("stray.toml"), "").unwrap();

        let names: Vec<_> = scan_dir(root.path()).into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert!(scan_dir(&root.path().join("missing")).is_empty());
    }

    #[test]
    fn project_shadows_user() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let project_dir = project_plugins_dir(project.path());
        write_manifest(&project_dir, "x", "shared", "project");
        write_manifest(user.path(), "x", "shared", "user");
        write_manifest(user.path(), "y", "only-user", "user");

        let found = discover_in(&[project_dir, user.path().to_path_buf()]);
        let pairs: Vec<_> = found
            .iter()
            .map(|m| (m.name.as_str(), m.version.as_str()))
            .collect();
        assert_eq!(pairs, vec![("shared", "project"), ("only-user", "user")]);
    }

    #[test]
    fn discover_reads_project_dir() {
        let project = tempfile::tempdir().unwrap();
        write_manifest(
            &project_plugins_dir(project.path()),
            "p",
            "vibe-test-unique-plugin",
            "1",
        );
        assert!(
            discover(project.path())
                .iter()
                .any(|m| m.name == "vibe-test-unique-plugin")
        );
    }

    #[test]
    fn inline_config_wins() {
        let root = tempfile::tempdir().unwrap();
        write_manifest(root.path(), "a", "a", "1");
        write_manifest(root.path(), "b", "b", "1");
        let discovered = scan_dir(root.path());
        let mut config = VibeConfig::default();
        config.plugins.push(PluginConfig {
            name: "b".into(),
            command: vec!["inline-b".into()],
            env: Default::default(),
            cwd: None,
            capabilities: Vec::new(),
            required: true,
        });
        let merged = merge_with_config(&discovered, &config);
        let summary: Vec<_> = merged
            .iter()
            .map(|c| (c.name.as_str(), c.command[0].as_str()))
            .collect();
        assert_eq!(summary, vec![("b", "inline-b"), ("a", "run-a")]);
        assert_eq!(plugin_configs(root.path(), &config)[0].name, "b");
    }
}
