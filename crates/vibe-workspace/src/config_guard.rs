//! Detecting tampering with the project's git configuration.
//!
//! Git worktrees share the repository configuration with the user's
//! checkout, and an agent can run `git config` from its worktree. Several
//! configuration keys make git execute arbitrary commands later, in the
//! user's checkout: `core.hooksPath`, `filter.<driver>.clean` / `smudge`
//! (combined with a `.gitattributes` the agent writes), `diff.external`,
//! `gpg.program`, `credential.helper`, `submodule.<name>.update = !cmd`,
//! `include.path`, and more. Rather than chase a list of dangerous keys,
//! the framework snapshots the whole repository-local configuration when a
//! workspace is opened and refuses to merge if anything outside the
//! explicitly allowed volatile keys ([`is_volatile_key`]) changed.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use vibe_core::{Error, Result};

use crate::git::Git;

/// Prefix of keys captured from the per-worktree configuration of the
/// project checkout (`git config --worktree`, only when
/// `extensions.worktreeConfig` is enabled).
pub const WORKTREE_SCOPE_PREFIX: &str = "worktree:";

/// Whether a configuration key may change between `open` and `merge`
/// without being treated as tampering. The allowed keys are exactly:
///
/// * `branch.vibe/<anything>.*`: sections of the framework's own task
///   branches, which the framework writes (`vibebase`) and git creates or
///   deletes as tasks are opened and discarded, possibly concurrently;
/// * `core.repositoryformatversion`: bumped by git itself when it enables a
///   repository extension.
///
/// Everything else, including `extensions.*`, `include.*`, `remote.*` and
/// `submodule.*`, must stay identical.
#[must_use]
pub fn is_volatile_key(key: &str) -> bool {
    let key = key.strip_prefix(WORKTREE_SCOPE_PREFIX).unwrap_or(key);
    key.starts_with("branch.vibe/") || key.eq_ignore_ascii_case("core.repositoryformatversion")
}

/// How a key differs between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The key did not exist in the baseline.
    Added,
    /// The key no longer exists.
    Removed,
    /// The key exists in both but its value(s) differ.
    Modified,
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
        })
    }
}

/// One configuration key that differs from the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChange {
    /// The key, as git lists it (section and name lower-cased).
    pub key: String,
    /// What happened to it.
    pub kind: ChangeKind,
    /// Current value(s), empty when the key was removed.
    pub current: Vec<String>,
}

/// Repository-local git configuration at one point in time: every key with
/// all of its values, in file order. A key written without a value
/// (`[x] y`) is recorded with the value `true`, matching git's
/// interpretation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSnapshot {
    entries: BTreeMap<String, Vec<String>>,
}

impl ConfigSnapshot {
    /// Capture the repository-local configuration of the repository at
    /// `git.root()`, plus its per-worktree configuration when
    /// `extensions.worktreeConfig` is enabled. The framework's `-c`
    /// overrides are not applied, so they never appear in the snapshot.
    pub async fn capture(git: &Git) -> Result<Self> {
        let mut snapshot = Self::parse_z(&git.config_list_raw("--local").await?, "");
        let worktree_config = snapshot
            .entries
            .get("extensions.worktreeconfig")
            .and_then(|v| v.last())
            .is_some_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")
            });
        if worktree_config {
            let wt = Self::parse_z(
                &git.config_list_raw("--worktree").await?,
                WORKTREE_SCOPE_PREFIX,
            );
            snapshot.entries.extend(wt.entries);
        }
        Ok(snapshot)
    }

    /// Parse `git config --list -z` output, prefixing every key.
    #[must_use]
    pub fn parse_z(raw: &str, prefix: &str) -> Self {
        let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in raw.split('\0').filter(|e| !e.is_empty()) {
            let (key, value) = entry.split_once('\n').unwrap_or((entry, "true"));
            entries
                .entry(format!("{prefix}{key}"))
                .or_default()
                .push(value.to_string());
        }
        Self { entries }
    }

    /// Values of `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.entries.get(key).map(Vec::as_slice)
    }

    /// Number of distinct keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshot holds no key.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Keys that differ between `baseline` (`self`) and `current`, ignoring
    /// [volatile keys](is_volatile_key), sorted by key.
    #[must_use]
    pub fn changes_to(&self, current: &ConfigSnapshot) -> Vec<ConfigChange> {
        let mut changes = Vec::new();
        for (key, now) in &current.entries {
            if is_volatile_key(key) {
                continue;
            }
            match self.entries.get(key) {
                None => changes.push(ConfigChange {
                    key: key.clone(),
                    kind: ChangeKind::Added,
                    current: now.clone(),
                }),
                Some(before) if before != now => changes.push(ConfigChange {
                    key: key.clone(),
                    kind: ChangeKind::Modified,
                    current: now.clone(),
                }),
                Some(_) => {}
            }
        }
        for key in self.entries.keys() {
            if !is_volatile_key(key) && !current.entries.contains_key(key) {
                changes.push(ConfigChange {
                    key: key.clone(),
                    kind: ChangeKind::Removed,
                    current: Vec::new(),
                });
            }
        }
        changes.sort_by(|a, b| a.key.cmp(&b.key));
        changes
    }

    /// Serialise to JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.entries)?)
    }

    /// Parse the JSON produced by [`ConfigSnapshot::to_json`].
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(Self {
            entries: serde_json::from_str(json)?,
        })
    }

    /// Read a snapshot file. `Ok(None)` when it does not exist.
    pub async fn load(path: &Path) -> Result<Option<Self>> {
        match tokio::fs::read_to_string(path).await {
            Ok(json) => Self::from_json(&json).map(Some).map_err(|e| {
                Error::workspace(format!(
                    "configuration snapshot {} is corrupt: {}",
                    path.display(),
                    e.message
                ))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Write the snapshot to `path`, creating parent directories.
    pub async fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, self.to_json()?).await?;
        Ok(())
    }
}

/// Build the error reported when the configuration changed. When
/// `core.hooksPath` is among the changes, the message leads with it.
#[must_use]
pub fn tampering_error(branch: &str, repo: &Path, changes: &[ConfigChange]) -> Error {
    let listed = changes
        .iter()
        .map(|c| format!("{} ({})", c.key, c.kind))
        .collect::<Vec<_>>()
        .join(", ");
    let hooks = changes
        .iter()
        .find(|c| c.key.trim_start_matches(WORKTREE_SCOPE_PREFIX) == "core.hookspath");
    let lead = match hooks {
        Some(c) if c.kind == ChangeKind::Removed => {
            "core.hooksPath was removed from the repository config".to_string()
        }
        Some(c) => format!(
            "core.hooksPath was changed to {:?} in the repository config",
            c.current.last().map_or("", String::as_str)
        ),
        None => "the repository config changed".to_string(),
    };
    Error::workspace(format!(
        "refusing to merge {branch}: {lead} at {} since the workspace was opened; this looks \
         like tampering by the task's agent. Changed keys: {listed}. Review the repository \
         config (`git config --local --list`), revert these changes, or accept them with \
         GitWorktreeProvider::accept_config_changes",
        repo.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn snap(raw: &str) -> ConfigSnapshot {
        ConfigSnapshot::parse_z(raw, "")
    }

    #[test]
    fn parses_nul_separated_listing() {
        let s = snap("core.bare\nfalse\0remote.origin.fetch\na\0remote.origin.fetch\nb\0x.flag\0");
        assert_eq!(s.get("core.bare"), Some(&["false".to_string()][..]));
        assert_eq!(s.get("remote.origin.fetch").unwrap().len(), 2);
        assert_eq!(s.get("x.flag"), Some(&["true".to_string()][..]));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn values_with_newlines_survive() {
        let s = snap("alias.x\n!echo a\nb\0");
        assert_eq!(s.get("alias.x"), Some(&["!echo a\nb".to_string()][..]));
    }

    #[test]
    fn detects_added_modified_removed_and_ignores_volatile() {
        let before = snap(
            "core.bare\nfalse\0core.repositoryformatversion\n0\0user.name\nA\0branch.vibe/t.vibebase\nmain\0",
        );
        let after = snap(
            "core.repositoryformatversion\n1\0user.name\nB\0filter.x.clean\nevil\0branch.vibe/u.vibebase\nmain\0",
        );
        let changes = before.changes_to(&after);
        let summary: Vec<_> = changes.iter().map(|c| (c.key.as_str(), c.kind)).collect();
        assert_eq!(
            summary,
            vec![
                ("core.bare", ChangeKind::Removed),
                ("filter.x.clean", ChangeKind::Added),
                ("user.name", ChangeKind::Modified),
            ]
        );
        assert!(before.changes_to(&before).is_empty());
    }

    #[test]
    fn volatile_keys_are_explicit() {
        assert!(is_volatile_key("branch.vibe/add-x-1234abcd.vibebase"));
        assert!(is_volatile_key("worktree:branch.vibe/t.remote"));
        assert!(is_volatile_key("core.repositoryformatversion"));
        assert!(!is_volatile_key("branch.main.remote"));
        assert!(!is_volatile_key("extensions.worktreeconfig"));
        assert!(!is_volatile_key("core.hookspath"));
    }

    #[test]
    fn json_roundtrip() {
        let s = snap("a.b\n1\0a.b\n2\0c.d\nx\ny\0");
        assert_eq!(ConfigSnapshot::from_json(&s.to_json().unwrap()).unwrap(), s);
    }

    #[test]
    fn hooks_path_leads_the_message() {
        let before = snap("user.name\nA\0");
        let after = snap("user.name\nA\0core.hookspath\n/evil\0filter.x.clean\nrm\0");
        let err = tampering_error("vibe/t", Path::new("repo"), &before.changes_to(&after));
        assert!(
            err.message
                .contains("core.hooksPath was changed to \"/evil\"")
        );
        assert!(err.message.contains("filter.x.clean (added)"));
        assert!(err.message.contains("tampering"));

        let err = tampering_error("vibe/t", Path::new("repo"), &after.changes_to(&before));
        assert!(err.message.starts_with("refusing to merge vibe/t"));
        assert!(err.message.contains("core.hooksPath was removed"));
    }
}
