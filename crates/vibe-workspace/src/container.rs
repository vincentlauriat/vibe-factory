//! Running agents' shell commands in a container.
//!
//! [`ContainerWorkspace`] (`"container"`) is a [`GitWorktreeProvider`] whose
//! shell commands run in a throw-away container instead of on the host:
//!
//! * **Files**: the task's git worktree on the host stays the single source
//!   of truth. The file tools (`read_file`, `write_file`, `edit_file`, …),
//!   the diff shown to reviewers, commits, merges and validated merges work
//!   on it exactly as with `git_worktree`.
//! * **Commands**: the `bash` tool (and therefore the pipeline's required
//!   validation commands) goes through a [`ContainerRunner`]. Each command
//!   runs in a fresh `docker run --rm` (or `podman run --rm`) container in
//!   which the workspace root is bind-mounted at [`CONTAINER_WORKDIR`]
//!   (`/workspace`) and the working directory is translated accordingly.
//!
//! ## One container per command
//!
//! A container is started for every command and removed when it ends,
//! rather than one long-lived container per task driven with `exec`:
//!
//! * a timeout or a cancelled tool call is handled by removing the
//!   container (`<runtime> rm -f <name>`), which kills the whole process
//!   tree, background jobs included; killing a `docker exec` client leaves
//!   its process running in the container;
//! * there is no container lifecycle to keep in sync with the task: nothing
//!   to restart on resume, nothing leaked when the host process dies
//!   between commands, and the integration candidate of a validated merge
//!   (a separate temporary worktree) is handled like any other workspace;
//! * nothing but the workspace survives between commands, which is also
//!   what the host `bash` tool guarantees.
//!
//! The price is the start-up time of a container per command (typically a
//! few hundred milliseconds).
//!
//! ## Hardening
//!
//! Every container is started with [`HARDENING_FLAGS`] (`--cap-drop=ALL`,
//! `--security-opt=no-new-privileges`, `--read-only`), a `/tmp` tmpfs,
//! `--rm`, a process limit, and `--network=none` unless a network is both
//! configured and granted. The configuration ([`ContainerConfig`]) has no
//! way to pass raw runtime flags; every value is validated
//! ([`ContainerSettings::from_config`]) before any container starts.
//!
//! The worktree's `.git` entry is bind-mounted read-only over itself, so a
//! command cannot replace the pointer to the repository with a crafted git
//! directory that the framework's host-side git commands would then use.
//! The repository's git directory itself is not mounted unless
//! `mount_git_metadata = true`, and then only read-only: git metadata is
//! written by the framework on the host.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vibe_core::config::{ContainerConfig, ContainerMount};
use vibe_core::workspace::MergeOutcome;
use vibe_core::{
    CommandRequest, CommandRunner, Error, MergeValidator, PreparedCommand, Result,
    SharedCommandRunner, Task, Workspace, WorkspaceKind, WorkspaceProvider,
};

use crate::worktree::GitWorktreeProvider;

/// Name of the provider, for `pipeline.workspace`.
pub const PROVIDER_NAME: &str = "container";

/// Where the workspace root is mounted inside the container.
pub const CONTAINER_WORKDIR: &str = "/workspace";

/// Flags every container is started with, whatever the configuration.
pub const HARDENING_FLAGS: &[&str] = &[
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--read-only",
];

/// Exit status of `docker run` / `podman run` when the runtime itself
/// failed (daemon unavailable, unknown image, invalid option, …).
pub const RUNNER_ERROR_EXIT_CODE: i32 = 125;

/// Label set on every container the framework starts.
pub const CONTAINER_LABEL: &str = "io.vibe-factory.managed=true";

/// Environment every command gets (after the passthrough variables, so
/// these win).
const FIXED_ENV: &[&str] = &[
    "HOME=/tmp",
    "GIT_TERMINAL_PROMPT=0",
    "GIT_PAGER=cat",
    "PAGER=cat",
];

/// Container paths that extra mounts may not cover.
const RESERVED_TARGETS: &[&str] = &["/proc", "/sys", "/dev", "/tmp", CONTAINER_WORKDIR];

/// The container runtime family, which decides a few option spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFlavor {
    /// Docker (or a Docker-compatible CLI named `docker`).
    Docker,
    /// Podman.
    Podman,
}

/// Validated network setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerNetwork {
    /// No network at all (`--network=none`).
    None,
    /// The runtime's default bridge.
    Bridge,
    /// A user-defined network, by name.
    Named(String),
}

impl ContainerNetwork {
    /// Parse a configured value. `host`, `container:<id>` and any other
    /// namespace-sharing mode are refused.
    pub fn parse(value: &str) -> Result<Self> {
        let v = value.trim();
        match v {
            "none" => return Ok(Self::None),
            "bridge" => return Ok(Self::Bridge),
            _ => {}
        }
        if v.eq_ignore_ascii_case("host") || v.contains(':') {
            return Err(Error::config(format!(
                "workspace.container.network `{value}` shares a namespace with the host or \
                 another container; use `none`, `bridge` or a named network"
            )));
        }
        if !is_name(v, 128) {
            return Err(Error::config(format!(
                "workspace.container.network `{value}` is not `none`, `bridge` or a valid \
                 network name"
            )));
        }
        Ok(Self::Named(v.to_string()))
    }

    fn as_arg(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Bridge => "bridge",
            Self::Named(n) => n,
        }
    }
}

/// A validated extra bind mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    /// Canonical host path.
    pub source: String,
    /// Absolute container path.
    pub target: String,
    /// Mounted read-only.
    pub read_only: bool,
}

/// A validated [`ContainerConfig`]: everything needed to build the runtime's
/// command line.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSettings {
    /// Runtime program (`docker`, `podman`, or a path to one).
    pub runtime: String,
    /// Runtime family.
    pub flavor: RuntimeFlavor,
    /// Image reference.
    pub image: String,
    /// Configured network.
    pub network: ContainerNetwork,
    /// Extra mounts.
    pub mounts: Vec<BindMount>,
    /// `--cpus`.
    pub cpus: Option<f64>,
    /// `--memory` (and `--memory-swap`).
    pub memory: Option<String>,
    /// `--pids-limit`.
    pub pids_limit: u32,
    /// Size of the `/tmp` tmpfs.
    pub tmp_size: Option<String>,
    /// Explicit `--user`; `None` uses the owner of the workspace on Unix.
    pub user: Option<String>,
    /// Host variables passed through.
    pub env: Vec<String>,
    /// Mount the repository's git directory read-only.
    pub mount_git_metadata: bool,
}

/// Everything that varies between two commands of the same settings.
#[derive(Debug, Clone, Copy)]
pub struct RunSpec<'a> {
    /// Container name.
    pub name: &'a str,
    /// Host path of the workspace root (mount source).
    pub workspace: &'a str,
    /// Working directory inside the container.
    pub workdir: &'a str,
    /// Shell command line.
    pub command: &'a str,
    /// Whether the invocation was granted network access.
    pub network_granted: bool,
    /// `--user` value, if any.
    pub user: Option<&'a str>,
    /// Host path of the workspace's `.git` entry, mounted read-only over
    /// itself.
    pub git_pointer: Option<&'a str>,
    /// Host path of the repository's git directory, mounted read-only at the
    /// same path.
    pub git_metadata: Option<&'a str>,
}

impl ContainerSettings {
    /// Validate `config` for the project at `project_root`.
    ///
    /// Refused: an empty or option-like image, an unknown runtime, a
    /// namespace-sharing network, limits that are not positive, malformed
    /// sizes, users or variable names, and mounts whose source is missing,
    /// is the filesystem root or a socket, or (for writable mounts) overlaps
    /// the project's `.git` or `.vibe` directory, or whose target is not an
    /// absolute container path outside `/workspace`, `/tmp`, `/proc`, `/sys`
    /// and `/dev`.
    pub fn from_config(config: &ContainerConfig, project_root: &Path) -> Result<Self> {
        let runtime = config.runtime.trim().to_string();
        let flavor = runtime_flavor(&runtime)?;
        let image = config.image.trim().to_string();
        if image.is_empty() {
            return Err(Error::config(
                "workspace.container.image is required (e.g. `image = \"rust:1.88\"`)",
            ));
        }
        if image.starts_with('-') || image.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::config(format!(
                "workspace.container.image `{image}` is not a valid image reference"
            )));
        }
        let network = ContainerNetwork::parse(&config.network)?;
        if let Some(cpus) = config.cpus
            && !(cpus.is_finite() && cpus > 0.0)
        {
            return Err(Error::config(format!(
                "workspace.container.cpus must be a positive number, got {cpus}"
            )));
        }
        if config.pids_limit == 0 {
            return Err(Error::config(
                "workspace.container.pids_limit must be at least 1",
            ));
        }
        let memory = validate_size("memory", config.memory.as_deref())?;
        let tmp_size = validate_size("tmp_size", config.tmp_size.as_deref())?;
        let user = match config.user.as_deref().map(str::trim) {
            None => None,
            Some(u) if is_user(u) => Some(u.to_string()),
            Some(u) => {
                return Err(Error::config(format!(
                    "workspace.container.user `{u}` must be `uid[:gid]` or `name[:group]`"
                )));
            }
        };
        for name in &config.env {
            if !is_env_name(name) {
                return Err(Error::config(format!(
                    "workspace.container.env entry `{name}` is not a variable name (list names \
                     only; values come from the host environment)"
                )));
            }
        }
        if config.mount_git_metadata && cfg!(windows) {
            return Err(Error::config(
                "workspace.container.mount_git_metadata is not supported on Windows hosts",
            ));
        }
        let mut mounts: Vec<BindMount> = Vec::new();
        for m in &config.mounts {
            let mount = validate_mount(m, project_root)?;
            if mounts.iter().any(|o| o.target == mount.target) {
                return Err(Error::config(format!(
                    "workspace.container.mounts: target `{}` is mounted twice",
                    mount.target
                )));
            }
            mounts.push(mount);
        }
        Ok(Self {
            runtime,
            flavor,
            image,
            network,
            mounts,
            cpus: config.cpus,
            memory,
            pids_limit: config.pids_limit,
            tmp_size,
            user,
            env: config.env.clone(),
            mount_git_metadata: config.mount_git_metadata,
        })
    }

    /// Network actually attached: the configured one when the invocation
    /// was granted network access, `none` otherwise.
    #[must_use]
    pub fn effective_network(&self, granted: bool) -> &str {
        if granted {
            self.network.as_arg()
        } else {
            "none"
        }
    }

    /// Arguments of `<runtime>` (the program itself excluded) running
    /// `spec`. Pure: no file system or environment access.
    #[must_use]
    pub fn run_args(&self, spec: &RunSpec<'_>) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "--rm".into(),
            format!("--name={}", spec.name),
            format!("--label={CONTAINER_LABEL}"),
        ];
        args.extend(HARDENING_FLAGS.iter().map(|f| (*f).to_string()));
        let mut tmpfs = String::from("/tmp:rw,exec,nosuid,nodev");
        if let Some(size) = &self.tmp_size {
            tmpfs.push_str(&format!(",size={size}"));
        }
        args.push(format!("--tmpfs={tmpfs}"));
        args.push(format!(
            "--network={}",
            self.effective_network(spec.network_granted)
        ));
        args.push(format!("--pids-limit={}", self.pids_limit));
        if let Some(cpus) = self.cpus {
            args.push(format!("--cpus={cpus}"));
        }
        if let Some(memory) = &self.memory {
            args.push(format!("--memory={memory}"));
            args.push(format!("--memory-swap={memory}"));
        }
        if let Some(user) = spec.user {
            args.push(format!("--user={user}"));
        }
        args.push(bind_arg(spec.workspace, CONTAINER_WORKDIR, false));
        if let Some(pointer) = spec.git_pointer {
            args.push(bind_arg(
                pointer,
                &format!("{CONTAINER_WORKDIR}/.git"),
                true,
            ));
        }
        if let Some(git_dir) = spec.git_metadata {
            args.push(bind_arg(git_dir, git_dir, true));
        }
        for m in &self.mounts {
            args.push(bind_arg(&m.source, &m.target, m.read_only));
        }
        args.push(format!("--workdir={}", spec.workdir));
        for name in &self.env {
            args.push("--env".into());
            args.push(name.clone());
        }
        for pair in FIXED_ENV {
            args.push("--env".into());
            args.push((*pair).to_string());
        }
        args.push(self.image.clone());
        args.extend(["sh".into(), "-c".into(), spec.command.to_string()]);
        args
    }

    /// Command removing the container `name` (and killing what runs in it).
    #[must_use]
    pub fn cleanup_command(&self, name: &str) -> Vec<String> {
        vec![
            self.runtime.clone(),
            "rm".into(),
            "--force".into(),
            name.to_string(),
        ]
    }
}

fn bind_arg(source: &str, target: &str, read_only: bool) -> String {
    let mut arg = format!("--mount=type=bind,source={source},target={target}");
    if read_only {
        arg.push_str(",readonly");
    }
    arg
}

fn runtime_flavor(runtime: &str) -> Result<RuntimeFlavor> {
    let stem = Path::new(runtime)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match stem {
        "docker" if !runtime.starts_with('-') => Ok(RuntimeFlavor::Docker),
        "podman" if !runtime.starts_with('-') => Ok(RuntimeFlavor::Podman),
        _ => Err(Error::config(format!(
            "workspace.container.runtime `{runtime}` is not supported (use `docker` or `podman`, \
             or a path to one of them)"
        ))),
    }
}

/// `[A-Za-z0-9][A-Za-z0-9_.-]*`, at most `max` characters.
fn is_name(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn is_user(s: &str) -> bool {
    let mut parts = s.split(':');
    let ok = |p: Option<&str>| p.is_some_and(|p| is_name(p, 64));
    match (parts.next(), parts.next(), parts.next()) {
        (user, None, None) => ok(user),
        (user, Some(group), None) => ok(user) && ok(Some(group)),
        _ => false,
    }
}

fn is_env_name(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A size such as `512m` or `4g`: digits and an optional `b`/`k`/`m`/`g`.
fn validate_size(field: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim) else {
        return Ok(None);
    };
    let digits =
        value.trim_end_matches(|c: char| matches!(c.to_ascii_lowercase(), 'b' | 'k' | 'm' | 'g'));
    let unit_len = value.len() - digits.len();
    let valid = unit_len <= 1
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits.parse::<u64>().is_ok_and(|n| n > 0);
    if valid {
        Ok(Some(value.to_ascii_lowercase()))
    } else {
        Err(Error::config(format!(
            "workspace.container.{field} `{value}` must be a positive size such as `512m` or `4g`"
        )))
    }
}

/// Host path as passed to the runtime: UTF-8, without Windows' verbatim
/// prefix, and free of the characters that would break `--mount`'s
/// comma-separated syntax.
pub fn host_path_arg(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        Error::workspace(format!(
            "path {} is not valid UTF-8 and cannot be mounted",
            path.display()
        ))
    })?;
    let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
    };
    if text
        .chars()
        .any(|c| matches!(c, ',' | '"') || c.is_control())
    {
        return Err(Error::workspace(format!(
            "path `{text}` contains a comma, quote or control character and cannot be \
             bind-mounted"
        )));
    }
    Ok(text)
}

/// Container path of `cwd`, a host directory inside `root`.
pub fn container_path(root: &Path, cwd: &Path) -> Result<String> {
    let relative = cwd.strip_prefix(root).map_err(|_| {
        Error::denied(format!(
            "working directory {} is outside the workspace {}",
            cwd.display(),
            root.display()
        ))
    })?;
    let mut out = String::from(CONTAINER_WORKDIR);
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| {
                    Error::workspace(format!("path {} is not valid UTF-8", cwd.display()))
                })?;
                if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
                    return Err(Error::workspace(format!(
                        "path component `{name}` cannot be translated to a container path"
                    )));
                }
                out.push('/');
                out.push_str(name);
            }
            Component::CurDir => {}
            _ => {
                return Err(Error::denied(format!(
                    "working directory {} cannot be translated to a container path",
                    cwd.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Lexically normalise `.` and `..` components.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalise(path))
}

fn validate_mount(mount: &ContainerMount, project_root: &Path) -> Result<BindMount> {
    let joined = if mount.source.is_absolute() {
        mount.source.clone()
    } else {
        project_root.join(&mount.source)
    };
    let source = joined.canonicalize().map_err(|e| {
        Error::config(format!(
            "workspace.container.mounts: source {} does not exist or is not accessible: {e}",
            joined.display()
        ))
    })?;
    if source.parent().is_none() {
        return Err(Error::config(
            "workspace.container.mounts: the filesystem root cannot be mounted",
        ));
    }
    let file_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if file_name.ends_with(".sock") || is_socket(&source) {
        return Err(Error::config(format!(
            "workspace.container.mounts: {} is a socket; mounting a runtime socket gives the \
             container control of the host",
            source.display()
        )));
    }
    if !mount.read_only {
        let project = canonical_or_lexical(project_root);
        for protected in [".git", ".vibe"] {
            let dir = canonical_or_lexical(&project.join(protected));
            if overlaps(&source, &dir) {
                return Err(Error::config(format!(
                    "workspace.container.mounts: {} overlaps the project's {protected} directory \
                     and can only be mounted read-only",
                    source.display()
                )));
            }
        }
    }
    let target = mount.target.trim();
    let well_formed = target.starts_with('/')
        && target.len() > 1
        && !target.ends_with('/')
        && !target.contains("//")
        && !target.contains('\\')
        && !target
            .split('/')
            .any(|segment| segment == "." || segment == "..");
    if !well_formed {
        return Err(Error::config(format!(
            "workspace.container.mounts: target `{target}` must be an absolute container path \
             such as `/cache` (not `/`, no `.`/`..` segments)"
        )));
    }
    if let Some(reserved) = RESERVED_TARGETS
        .iter()
        .find(|r| target == **r || target.starts_with(&format!("{r}/")))
    {
        return Err(Error::config(format!(
            "workspace.container.mounts: target `{target}` is inside the reserved `{reserved}`"
        )));
    }
    let source = host_path_arg(&source).map_err(|e| Error::config(e.message))?;
    if target
        .chars()
        .any(|c| matches!(c, ',' | '"') || c.is_control())
    {
        return Err(Error::config(format!(
            "workspace.container.mounts: target `{target}` contains a comma, quote or control \
             character"
        )));
    }
    Ok(BindMount {
        source,
        target: target.to_string(),
        read_only: mount.read_only,
    })
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_socket(_path: &Path) -> bool {
    false
}

/// `uid:gid` owning `root`, the default container user on Unix hosts so
/// that files written in the container belong to the host user.
#[cfg(unix)]
fn default_user(root: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(root)
        .ok()
        .map(|m| format!("{}:{}", m.uid(), m.gid()))
}

#[cfg(not(unix))]
fn default_user(_root: &Path) -> Option<String> {
    None
}

/// The repository's common git directory for the workspace at `root`, as
/// the workspace's `.git` refers to it (so that the path resolves the same
/// way inside the container once mounted at the same place).
pub fn git_common_dir(root: &Path) -> Result<PathBuf> {
    let dot_git = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).map_err(|e| {
        Error::workspace(format!(
            "{} has no .git entry, so its git metadata cannot be mounted: {e}",
            root.display()
        ))
    })?;
    if meta.is_dir() {
        return Ok(normalise(&dot_git));
    }
    let text = std::fs::read_to_string(&dot_git)?;
    let gitdir = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .ok_or_else(|| Error::workspace(format!("{} is not a gitdir file", dot_git.display())))?;
    let gitdir = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        root.join(gitdir)
    };
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) if Path::new(rel.trim()).is_absolute() => PathBuf::from(rel.trim()),
        Ok(rel) => gitdir.join(rel.trim()),
        Err(_) => gitdir,
    };
    Ok(normalise(&common))
}

fn unique_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!(
        "vibe-{}-{nanos:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// [`CommandRunner`] starting one container per command.
#[derive(Debug, Clone)]
pub struct ContainerRunner {
    settings: Arc<ContainerSettings>,
}

impl ContainerRunner {
    /// Runner for validated settings.
    #[must_use]
    pub fn new(settings: ContainerSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }

    /// The settings commands run with.
    #[must_use]
    pub fn settings(&self) -> &ContainerSettings {
        &self.settings
    }
}

impl CommandRunner for ContainerRunner {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn note(&self) -> Option<String> {
        Some(format!(
            "Commands run in an isolated `{}` container: the workspace is mounted at \
             `{CONTAINER_WORKDIR}` (the default working directory), nothing outside it persists \
             between commands, the root file system is read-only (use `/tmp` for scratch \
             files), and the network is {}. Refer to files with paths relative to the \
             workspace.",
            self.settings.image,
            match self.settings.network {
                ContainerNetwork::None => "disabled".to_string(),
                _ => "only available when network access is granted".to_string(),
            }
        ))
    }

    fn prepare(&self, request: &CommandRequest<'_>) -> Result<PreparedCommand> {
        let workspace = host_path_arg(request.workspace_root)?;
        let workdir = container_path(request.workspace_root, request.cwd)?;
        let pointer = request.workspace_root.join(".git");
        let git_pointer = if std::fs::symlink_metadata(&pointer).is_ok() {
            Some(host_path_arg(&pointer)?)
        } else {
            None
        };
        let git_metadata = if self.settings.mount_git_metadata {
            Some(host_path_arg(&git_common_dir(request.workspace_root)?)?)
        } else {
            None
        };
        let user = self
            .settings
            .user
            .clone()
            .or_else(|| default_user(request.workspace_root));
        let name = unique_name();
        let args = self.settings.run_args(&RunSpec {
            name: &name,
            workspace: &workspace,
            workdir: &workdir,
            command: request.command,
            network_granted: request.network,
            user: user.as_deref(),
            git_pointer: git_pointer.as_deref(),
            git_metadata: git_metadata.as_deref(),
        });
        Ok(PreparedCommand {
            program: self.settings.runtime.clone(),
            args,
            current_dir: Some(request.workspace_root.to_path_buf()),
            cleanup: Some(self.settings.cleanup_command(&name)),
            runner_error_exit_code: Some(RUNNER_ERROR_EXIT_CODE),
        })
    }
}

/// Whether `runtime` answers `info` (the daemon, or Podman's service, is
/// reachable).
pub async fn runtime_available(runtime: &str) -> bool {
    tokio::process::Command::new(runtime)
        .arg("info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Workspace provider running shell commands in containers over git
/// worktrees. See the [module documentation](self).
#[derive(Debug, Clone)]
pub struct ContainerWorkspace {
    inner: GitWorktreeProvider,
    runner: ContainerRunner,
}

impl ContainerWorkspace {
    /// Wrap `inner` (which owns the worktrees, branches and merges).
    #[must_use]
    pub fn new(inner: GitWorktreeProvider, settings: ContainerSettings) -> Self {
        Self {
            inner,
            runner: ContainerRunner::new(settings),
        }
    }

    /// The runner the `bash` tool must use with this provider.
    #[must_use]
    pub fn runner(&self) -> SharedCommandRunner {
        Arc::new(self.runner.clone())
    }

    /// The container settings.
    #[must_use]
    pub fn settings(&self) -> &ContainerSettings {
        self.runner.settings()
    }

    /// The wrapped worktree provider.
    #[must_use]
    pub fn worktrees(&self) -> &GitWorktreeProvider {
        &self.inner
    }
}

/// Kind of the workspaces opened by [`ContainerWorkspace`].
#[must_use]
pub fn container_kind() -> WorkspaceKind {
    WorkspaceKind::Custom(PROVIDER_NAME.to_string())
}

struct Relabel<'a>(&'a mut dyn MergeValidator);

#[async_trait::async_trait]
impl MergeValidator for Relabel<'_> {
    async fn validate(&mut self, candidate: &Workspace) -> Result<()> {
        let mut candidate = candidate.clone();
        candidate.kind = container_kind();
        self.0.validate(&candidate).await
    }
}

#[async_trait::async_trait]
impl WorkspaceProvider for ContainerWorkspace {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    async fn open(&self, project_root: &Path, task: &Task) -> Result<Workspace> {
        let mut workspace = self.inner.open(project_root, task).await?;
        workspace.kind = container_kind();
        Ok(workspace)
    }

    async fn merge(&self, workspace: &Workspace) -> Result<MergeOutcome> {
        self.inner.merge(workspace).await
    }

    async fn merge_validated(
        &self,
        workspace: &Workspace,
        validator: &mut dyn MergeValidator,
    ) -> Result<MergeOutcome> {
        self.inner
            .merge_validated(workspace, &mut Relabel(validator))
            .await
    }

    async fn discard(&self, workspace: &Workspace) -> Result<()> {
        self.inner.discard(workspace).await
    }

    async fn changes(&self, workspace: &Workspace) -> Result<String> {
        self.inner.changes(workspace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(config: &ContainerConfig) -> Result<ContainerSettings> {
        let dir = tempfile::tempdir().unwrap();
        ContainerSettings::from_config(config, dir.path())
    }

    fn spec<'a>(network_granted: bool) -> RunSpec<'a> {
        RunSpec {
            name: "vibe-test",
            workspace: "/host/ws",
            workdir: "/workspace/src",
            command: "cargo test",
            network_granted,
            user: Some("1000:1000"),
            git_pointer: Some("/host/ws/.git"),
            git_metadata: None,
        }
    }

    #[test]
    fn run_args_are_hardened_and_exact() {
        let mut config = ContainerConfig::new("rust:1.88");
        config.cpus = Some(2.0);
        config.memory = Some("4G".into());
        config.env = vec!["CARGO_HOME".into()];
        let s = settings(&config).unwrap();
        let args = s.run_args(&spec(false));
        assert_eq!(
            args,
            vec![
                "run",
                "--rm",
                "--name=vibe-test",
                "--label=io.vibe-factory.managed=true",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--read-only",
                "--tmpfs=/tmp:rw,exec,nosuid,nodev",
                "--network=none",
                "--pids-limit=1024",
                "--cpus=2",
                "--memory=4g",
                "--memory-swap=4g",
                "--user=1000:1000",
                "--mount=type=bind,source=/host/ws,target=/workspace",
                "--mount=type=bind,source=/host/ws/.git,target=/workspace/.git,readonly",
                "--workdir=/workspace/src",
                "--env",
                "CARGO_HOME",
                "--env",
                "HOME=/tmp",
                "--env",
                "GIT_TERMINAL_PROMPT=0",
                "--env",
                "GIT_PAGER=cat",
                "--env",
                "PAGER=cat",
                "rust:1.88",
                "sh",
                "-c",
                "cargo test",
            ]
        );
        assert_eq!(
            s.cleanup_command("vibe-test"),
            vec!["docker", "rm", "--force", "vibe-test"]
        );
    }

    #[test]
    fn network_needs_both_configuration_and_permission() {
        let mut config = ContainerConfig::new("alpine");
        let none = settings(&config).unwrap();
        assert!(
            none.run_args(&spec(true))
                .contains(&"--network=none".to_string())
        );
        config.network = "bridge".into();
        let bridge = settings(&config).unwrap();
        assert!(
            bridge
                .run_args(&spec(false))
                .contains(&"--network=none".to_string())
        );
        assert!(
            bridge
                .run_args(&spec(true))
                .contains(&"--network=bridge".to_string())
        );
        config.network = "build-net".into();
        assert_eq!(
            settings(&config).unwrap().effective_network(true),
            "build-net"
        );
        for bad in ["host", "HOST", "container:abc", "ns:x", "-x", "a b"] {
            config.network = bad.into();
            assert!(settings(&config).is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_settings_are_refused() {
        let base = ContainerConfig::new("alpine");
        type Change = Box<dyn Fn(&mut ContainerConfig)>;
        let cases: Vec<(&str, Change)> = vec![
            ("empty image", Box::new(|c| c.image = " ".into())),
            (
                "option image",
                Box::new(|c| c.image = "--privileged".into()),
            ),
            ("runtime", Box::new(|c| c.runtime = "nerdctl".into())),
            ("cpus", Box::new(|c| c.cpus = Some(0.0))),
            ("nan cpus", Box::new(|c| c.cpus = Some(f64::NAN))),
            ("pids", Box::new(|c| c.pids_limit = 0)),
            ("memory", Box::new(|c| c.memory = Some("4 GB".into()))),
            ("tmp", Box::new(|c| c.tmp_size = Some("0m".into()))),
            ("user", Box::new(|c| c.user = Some("root;id".into()))),
            ("env", Box::new(|c| c.env = vec!["A=B".into()])),
        ];
        for (what, change) in cases {
            let mut c = base.clone();
            change(&mut c);
            assert!(settings(&c).is_err(), "{what} was accepted");
        }
        let mut podman = base.clone();
        podman.runtime = "/usr/bin/podman".into();
        assert_eq!(settings(&podman).unwrap().flavor, RuntimeFlavor::Podman);
    }

    #[test]
    fn mounts_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join("cache")).unwrap();
        let mount = |source: &str, target: &str, read_only: bool| {
            let mut c = ContainerConfig::new("alpine");
            c.mounts = vec![ContainerMount {
                source: PathBuf::from(source),
                target: target.into(),
                read_only,
            }];
            ContainerSettings::from_config(&c, project)
        };
        let ok = mount("cache", "/cache", false).unwrap();
        assert_eq!(ok.mounts[0].target, "/cache");
        assert!(!ok.mounts[0].read_only);
        assert!(mount("cache", "/cache", true).unwrap().mounts[0].read_only);
        // Read-only access to git metadata is allowed, write access is not.
        assert!(mount(".git", "/repo-git", true).is_ok());
        assert!(mount(".git", "/repo-git", false).is_err());
        assert!(mount("missing", "/m", true).is_err());
        for target in [
            "relative",
            "/",
            "/workspace",
            "/workspace/x",
            "/tmp/a",
            "/proc",
            "/a/../b",
            "/a,b",
        ] {
            assert!(mount("cache", target, true).is_err(), "{target}");
        }
        let mut twice = ContainerConfig::new("alpine");
        twice.mounts = vec![
            ContainerMount {
                source: "cache".into(),
                target: "/c".into(),
                read_only: true,
            },
            ContainerMount {
                source: ".git".into(),
                target: "/c".into(),
                read_only: true,
            },
        ];
        assert!(ContainerSettings::from_config(&twice, project).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_sockets_cannot_be_mounted() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("runtime.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let mut c = ContainerConfig::new("alpine");
        c.mounts = vec![ContainerMount {
            source: sock,
            target: "/var/run/docker.sock".into(),
            read_only: true,
        }];
        let err = ContainerSettings::from_config(&c, dir.path()).unwrap_err();
        assert!(err.message.contains("socket"), "{err}");
    }

    #[test]
    fn working_directories_are_translated() {
        let root = Path::new("/host/ws");
        assert_eq!(container_path(root, root).unwrap(), "/workspace");
        assert_eq!(
            container_path(root, &root.join("src").join("bin")).unwrap(),
            "/workspace/src/bin"
        );
        assert!(container_path(root, Path::new("/elsewhere")).is_err());
        assert!(host_path_arg(Path::new("/a,b")).is_err());
        assert_eq!(host_path_arg(Path::new("/a/b")).unwrap(), "/a/b");
    }

    #[test]
    fn runner_prepares_a_container_per_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        let mut config = ContainerConfig::new("alpine");
        config.user = Some("123:456".into());
        let runner = ContainerRunner::new(ContainerSettings::from_config(&config, &root).unwrap());
        let request = CommandRequest {
            command: "ls",
            workspace_root: &root,
            cwd: &root.join("src"),
            network: false,
        };
        let a = runner.prepare(&request).unwrap();
        let b = runner.prepare(&request).unwrap();
        assert_eq!(a.program, "docker");
        assert!(a.args.contains(&"--workdir=/workspace/src".to_string()));
        assert!(a.args.contains(&"--user=123:456".to_string()));
        assert_eq!(a.runner_error_exit_code, Some(RUNNER_ERROR_EXIT_CODE));
        let name = |p: &PreparedCommand| {
            p.args
                .iter()
                .find_map(|a| a.strip_prefix("--name="))
                .unwrap()
                .to_string()
        };
        assert_ne!(name(&a), name(&b), "container names are unique");
        assert_eq!(a.cleanup.as_ref().unwrap().last().unwrap(), &name(&a));
        assert!(runner.note().unwrap().contains("/workspace"));
        let outside = CommandRequest {
            cwd: Path::new("/"),
            ..request
        };
        assert!(runner.prepare(&outside).is_err());
    }

    #[test]
    fn git_common_dir_follows_worktree_pointers() {
        let dir = tempfile::tempdir().unwrap();
        let repo_git = dir.path().join("repo").join(".git");
        let wt_git = repo_git.join("worktrees").join("task");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(git_common_dir(&ws).unwrap(), normalise(&repo_git));
        assert_eq!(
            git_common_dir(&dir.path().join("repo")).unwrap(),
            normalise(&repo_git)
        );
    }
}
