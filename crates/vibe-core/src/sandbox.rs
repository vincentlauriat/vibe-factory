//! Running shell commands somewhere other than the host.
//!
//! The `bash` tool normally spawns `sh -c <command>` directly on the host, in
//! the workspace directory. A [`CommandRunner`] replaces that step: given the
//! command, the (host) workspace root and working directory, it returns the
//! process to spawn instead, for example
//! `docker run --rm … <image> sh -c <command>`. Everything else stays with
//! the tool: the security policy check, timeouts, output capture and
//! truncation, and the reporting of the exit code.
//!
//! The configuration of the built-in container runner
//! (`[workspace.container]`, see [`ContainerConfig`]) is declared here so
//! that it lives with the rest of [`crate::VibeConfig`]; the runner itself
//! is implemented by `vibe-workspace`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

/// One shell command the `bash` tool is about to run.
#[derive(Debug, Clone, Copy)]
pub struct CommandRequest<'a> {
    /// The shell command line, exactly as the agent wrote it.
    pub command: &'a str,
    /// Canonical host path of the workspace root.
    pub workspace_root: &'a Path,
    /// Canonical host path of the working directory, inside `workspace_root`.
    pub cwd: &'a Path,
    /// Whether the invocation was granted network access
    /// ([`crate::Permissions::network`]).
    pub network: bool,
}

/// The process that runs a [`CommandRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCommand {
    /// Program to spawn on the host (looked up in `PATH` when relative).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Host working directory of the spawned process (`None`: the
    /// request's `cwd`).
    pub current_dir: Option<PathBuf>,
    /// Command (program and arguments) run on the host once the process has
    /// ended, whether it exited, timed out or was cancelled, to release what
    /// it may have left behind (for a container: `docker rm -f <name>`).
    /// Best effort: failures are ignored.
    pub cleanup: Option<Vec<String>>,
    /// Exit status meaning "the runner itself failed" rather than "the
    /// command failed" (125 for `docker run`/`podman run`). Such a failure
    /// is reported with `sandbox_error: true` in the tool metadata.
    pub runner_error_exit_code: Option<i32>,
}

/// Turns a shell command into the process that runs it.
///
/// Implementations must not weaken the checks the caller already made: the
/// request's paths have been validated to stay inside the workspace.
pub trait CommandRunner: Send + Sync + fmt::Debug {
    /// Short name, reported in tool metadata (`"container"`).
    fn name(&self) -> &str;

    /// Sentence(s) appended to the `bash` tool description so the model
    /// knows where its commands run. Default: none.
    fn note(&self) -> Option<String> {
        None
    }

    /// Build the process for `request`. An error is reported to the agent
    /// as a failed tool call, without running anything.
    fn prepare(&self, request: &CommandRequest<'_>) -> Result<PreparedCommand>;
}

/// Shared handle to a command runner.
pub type SharedCommandRunner = Arc<dyn CommandRunner>;

/// Workspace settings (`[workspace]` in `.vibe/config.toml`). The provider
/// itself is still chosen by `pipeline.workspace`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceConfig {
    /// Settings of the `container` workspace provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerConfig>,
}

impl WorkspaceConfig {
    /// Whether nothing is configured (the table is then not serialised).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.container.is_none()
    }
}

/// Settings of the `container` workspace provider
/// (`[workspace.container]`).
///
/// Every field is explicit and validated by `vibe-workspace`; there is
/// deliberately no way to pass raw flags to the container runtime. Unknown
/// keys are rejected so that `privileged = true` or a typo cannot be
/// silently ignored.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    /// Container runtime: `docker` (default) or `podman`, or a path to one
    /// of them.
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Image the commands run in (required), e.g. `rust:1.88`.
    pub image: String,
    /// Network of the containers: `none` (default), `bridge`, or the name of
    /// an existing user-defined network. `host` and `container:<id>` are
    /// refused. The network is only attached when the agent was also
    /// granted network access (`security.allow_network`).
    #[serde(default = "default_network")]
    pub network: String,
    /// Extra bind mounts, read-only unless stated otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<ContainerMount>,
    /// CPU limit (`--cpus`), e.g. `2` or `1.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    /// Memory limit (`--memory`, also used as `--memory-swap`), e.g. `4g`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// Maximum number of processes (`--pids-limit`, default 1024).
    #[serde(default = "default_pids_limit")]
    pub pids_limit: u32,
    /// Size of the `/tmp` tmpfs, e.g. `1g` (default: the runtime's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmp_size: Option<String>,
    /// User the commands run as (`uid[:gid]` or a name). Default: the owner
    /// of the workspace directory on Unix hosts, the image's user elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Host environment variables passed to the container (none by
    /// default). Only names are listed; values come from the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Mount the repository's git directory read-only (at its host path) so
    /// that read-only git commands work inside the container. Unix hosts
    /// only. Default: false (git metadata stays on the host).
    #[serde(default)]
    pub mount_git_metadata: bool,
}

/// One extra bind mount of a [`ContainerConfig`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerMount {
    /// Host path (relative paths are resolved against the project root). It
    /// must exist.
    pub source: PathBuf,
    /// Absolute path inside the container.
    pub target: String,
    /// Mount read-only (default true).
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_runtime() -> String {
    "docker".to_string()
}

fn default_network() -> String {
    "none".to_string()
}

fn default_pids_limit() -> u32 {
    1024
}

fn default_read_only() -> bool {
    true
}

impl ContainerConfig {
    /// Configuration with every default and the given image.
    #[must_use]
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            runtime: default_runtime(),
            image: image.into(),
            network: default_network(),
            mounts: Vec::new(),
            cpus: None,
            memory: None,
            pids_limit: default_pids_limit(),
            tmp_size: None,
            user: None,
            env: Vec::new(),
            mount_git_metadata: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_config_defaults() {
        let cfg: WorkspaceConfig = toml::from_str("[container]\nimage = \"alpine\"\n").unwrap();
        assert_eq!(cfg.container, Some(ContainerConfig::new("alpine")));
        let c = cfg.container.unwrap();
        assert_eq!(c.runtime, "docker");
        assert_eq!(c.network, "none");
        assert_eq!(c.pids_limit, 1024);
    }

    #[test]
    fn container_config_rejects_unknown_keys_and_missing_image() {
        assert!(
            toml::from_str::<WorkspaceConfig>("[container]\nimage = \"a\"\nprivileged = true\n")
                .is_err()
        );
        assert!(toml::from_str::<WorkspaceConfig>("[container]\nnetwork = \"none\"\n").is_err());
    }

    #[test]
    fn vibe_config_roundtrips_the_container_table() {
        let text = "[pipeline]\nworkspace = \"container\"\n\n[workspace.container]\n\
                    image = \"rust:1.88\"\nnetwork = \"bridge\"\ncpus = 2.0\nmemory = \"4g\"\n";
        let cfg = crate::VibeConfig::from_toml(text).unwrap();
        let container = cfg.workspace.container.clone().unwrap();
        assert_eq!(container.image, "rust:1.88");
        assert_eq!(container.cpus, Some(2.0));
        let back = crate::VibeConfig::from_toml(&cfg.to_toml().unwrap()).unwrap();
        assert_eq!(back, cfg);
        // Absent table: not serialised.
        let default = crate::VibeConfig::default().to_toml().unwrap();
        assert!(!default.contains("[workspace"));
    }

    #[test]
    fn mounts_are_read_only_by_default() {
        let cfg: WorkspaceConfig = toml::from_str(
            "[container]\nimage = \"a\"\n[[container.mounts]]\nsource = \"x\"\ntarget = \"/x\"\n",
        )
        .unwrap();
        assert!(cfg.container.unwrap().mounts[0].read_only);
    }
}
