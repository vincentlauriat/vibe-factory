//! Denylist-based validation of shell commands.
//!
//! Every simple command found by the parser (see `parse_command`) is checked
//! in turn:
//!
//! 1. variables it sets (`PATH=… cmd`, `env LD_PRELOAD=… cmd`, `export`, …);
//! 2. files it writes through redirections (`> ~/x`, `>> /etc/…`);
//! 3. computed program names, blocked programs (including wrappers such as
//!    `nohup` or `strace` in front of them), the optional allowlist, network
//!    gating and name-defining builtins (`alias`, `hash`, `enable`);
//! 4. a per-program validator for commands that are useful but dangerous
//!    with some arguments (`rm`, `find`, `cd`, `chmod`, `git`, `kill`,
//!    shells, database clients, …);
//! 5. `xargs rm` pipelines, whose targets come from the commands feeding them.
//!
//! When a workspace root is known, paths that are removed, written or entered
//! must stay inside it.
//!
//! This is a best-effort guard against destructive mistakes, not a sandbox:
//! interpreters such as `python -c` can still do anything the operating
//! system allows. Run agents in an isolated workspace.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use vibe_core::config::SecurityConfig;

use super::command_parser::{CommandSegment, MAX_NESTING_DEPTH, normalize_program, parse_at_depth};

/// Programs that are always blocked: power management, disk formatting,
/// privilege escalation, ownership changes, firewalls, scanners, service and
/// user management.
pub const BLOCKED_PROGRAMS: &[&str] = &[
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "mkfs",
    "fdisk",
    "parted",
    "gdisk",
    "dd",
    "sudo",
    "su",
    "doas",
    "chown",
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "nmap",
    "systemctl",
    "service",
    "crontab",
    "mount",
    "umount",
    "useradd",
    "userdel",
    "usermod",
    "groupadd",
    "groupdel",
    "passwd",
    "visudo",
    "pkexec",
    "run0",
];

/// Programs that reach the network and therefore require network access.
pub const NETWORK_PROGRAMS: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "telnet", "ftp", "sftp", "scp", "ssh",
];

/// POSIX-like shells whose `-c` argument is validated recursively.
pub const SHELLS: &[&str] = &[
    "bash", "sh", "zsh", "fish", "dash", "ksh", "ash", "mksh", "tcsh", "csh", "rc", "elvish", "nu",
];

/// PowerShell executables whose `-Command` argument is validated recursively.
pub const POWERSHELLS: &[&str] = &["pwsh", "powershell"];

/// Database clients whose inline statements are inspected.
pub const DATABASE_CLIENTS: &[&str] =
    &["psql", "mysql", "mariadb", "redis-cli", "mongosh", "mongo"];

/// Process names `pkill` / `killall` may never target.
pub const SYSTEM_PROCESSES: &[&str] = &[
    "systemd",
    "launchd",
    "sshd",
    "dockerd",
    "init",
    "kernel_task",
    "windowserver",
    "explorer.exe",
];

/// A database or role name must contain one of these markers to be dropped.
pub const DISPOSABLE_NAME_MARKERS: &[&str] = &[
    "test", "dev", "local", "tmp", "temp", "scratch", "sandbox", "mock",
];

/// Harmless shell builtins accepted even when an allowlist is configured.
pub const ALWAYS_ALLOWED: &[&str] = &[
    "cd", "echo", "printf", "true", "false", "pwd", "test", "[", "exit", ":",
];

/// Builtins that can make a command name run a different program.
pub const NAME_DEFINING_BUILTINS: &[&str] = &["alias", "hash", "enable"];

/// Variables that may never be set by a command: they change which programs
/// run or inject code into them.
pub const FORBIDDEN_VARIABLES: &[&str] = &[
    "PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "IFS",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "CDPATH",
    "PERL5OPT",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "RUBYOPT",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_ASKPASS",
    "GIT_EXEC_PATH",
    "GIT_EXTERNAL_DIFF",
    "GIT_EDITOR",
    "GIT_PAGER",
];

/// Prefixes of variables that may never be set by a command.
pub const FORBIDDEN_VARIABLE_PREFIXES: &[&str] = &[
    "DYLD_",
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "GIT_AUTHOR_",
    "GIT_COMMITTER_",
];

/// Wrappers that are shell builtins or keywords; they are not subject to the
/// allowlist.
const TRANSPARENT_WRAPPERS: &[&str] = &["exec", "command", "builtin", "time"];

/// Builtins that set shell variables from `NAME=value` arguments.
const VARIABLE_SETTERS: &[&str] = &["export", "declare", "typeset", "readonly", "local"];

/// Builtins that set the variables named by their arguments.
const VARIABLE_READERS: &[&str] = &["read", "mapfile", "readarray", "getopts"];

/// Programs that delete the paths they are given.
const REMOVERS: &[&str] = &["rm", "rmdir", "unlink", "shred"];

/// Top-level directories that may not be removed or written, nor their
/// direct children, when no workspace root is known.
const PROTECTED_ROOTS: &[&str] = &[
    "home",
    "usr",
    "etc",
    "var",
    "bin",
    "lib",
    "opt",
    "sbin",
    "boot",
    "root",
    "dev",
    "sys",
    "proc",
    "users",
    "system",
    "library",
    "applications",
    "private",
];

/// Characters whose presence makes a path impossible to check statically.
const EXPANSION_CHARS: [char; 7] = ['$', '`', '*', '?', '[', '{', '~'];

/// Device files a redirection may always write to.
const DEVICE_SINKS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

/// `git -c` keys that could impersonate an author, pull in other
/// configuration or run arbitrary programs.
const GIT_FORBIDDEN_CONFIG_PREFIXES: &[&str] = &[
    "user.",
    "author.",
    "committer.",
    "alias.",
    "include.",
    "includeif.",
    "core.sshcommand",
    "core.pager",
    "core.editor",
    "core.hookspath",
    "core.fsmonitor",
    "core.gitproxy",
    "sequence.editor",
    "credential.helper",
    "diff.",
    "merge.",
    "filter.",
    "protocol.",
    "uploadpack.",
    "receive.",
    "core.askpass",
    "gpg.",
];

type Check = Result<(), String>;

/// Validates shell commands before they are executed.
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    blocked: BTreeSet<String>,
    allowed: BTreeSet<String>,
    allow_network: bool,
    workspace_root: Option<PathBuf>,
}

/// Per-validation settings passed down the recursive checks.
#[derive(Clone, Copy)]
struct Scope<'a> {
    network: bool,
    root: Option<&'a Path>,
}

impl SecurityPolicy {
    /// Build a policy from the project's security configuration.
    #[must_use]
    pub fn new(config: &SecurityConfig) -> Self {
        let mut blocked: BTreeSet<String> =
            BLOCKED_PROGRAMS.iter().map(|p| (*p).to_string()).collect();
        blocked.extend(config.blocked_commands.iter().map(|p| normalize_program(p)));
        let allowed = config
            .allowed_commands
            .iter()
            .map(|p| normalize_program(p))
            .collect();
        Self {
            blocked,
            allowed,
            allow_network: config.allow_network,
            workspace_root: None,
        }
    }

    /// Confine removed, written and entered paths to `root`.
    #[must_use]
    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// The workspace root paths are confined to, if any.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Whether the configuration itself grants network access.
    #[must_use]
    pub fn allows_network(&self) -> bool {
        self.allow_network
    }

    /// Check `command` using the configuration's network setting and the
    /// policy's workspace root.
    ///
    /// # Errors
    ///
    /// Returns a human-readable explanation, meant to be shown to the model,
    /// when the command is not allowed.
    pub fn validate(&self, command: &str) -> Result<(), String> {
        self.validate_with_network(command, false)
    }

    /// Check `command`, additionally granting network access when
    /// `network_permitted` is true (for example because the calling agent's
    /// permissions include the network).
    ///
    /// # Errors
    ///
    /// Returns a human-readable explanation when the command is not allowed.
    pub fn validate_with_network(
        &self,
        command: &str,
        network_permitted: bool,
    ) -> Result<(), String> {
        self.validate_command(command, network_permitted, self.workspace_root.as_deref())
    }

    /// Check `command` with an explicit workspace root (overriding the
    /// policy's own). Absolute paths that are removed, written through a
    /// redirection or entered with `cd` must be inside `workspace_root`.
    ///
    /// # Errors
    ///
    /// Returns a human-readable explanation when the command is not allowed.
    pub fn validate_command(
        &self,
        command: &str,
        network_permitted: bool,
        workspace_root: Option<&Path>,
    ) -> Result<(), String> {
        if command.trim().is_empty() {
            return Err("The command is empty.".into());
        }
        let scope = Scope {
            network: self.allow_network || network_permitted,
            root: workspace_root,
        };
        self.validate_depth(command, scope, 0)
    }

    fn validate_depth(&self, command: &str, scope: Scope<'_>, depth: usize) -> Check {
        if depth > MAX_NESTING_DEPTH {
            return Err("The command nests shells too deeply to be verified.".into());
        }
        let segments = parse_at_depth(command, depth).map_err(|e| {
            format!(
                "The command could not be parsed safely ({e}). Simplify it and avoid unusual \
                 shell syntax."
            )
        })?;
        for segment in &segments {
            self.validate_segment(segment, scope, depth)?;
        }
        check_xargs_removals(&segments, scope.root)
    }

    fn validate_segment(&self, seg: &CommandSegment, scope: Scope<'_>, depth: usize) -> Check {
        for (name, _) in &seg.assignments {
            check_variable(name)?;
        }
        for r in &seg.redirections {
            let target = r.target.as_str();
            if target.starts_with("/dev/tcp/") || target.starts_with("/dev/udp/") {
                if !scope.network {
                    return Err(format!(
                        "`{target}` opens a network connection, which is disabled for this agent."
                    ));
                }
                continue;
            }
            if r.writes_file() {
                check_path_target(target, scope.root, Purpose::Write)?;
                check_git_metadata("redirection", target)?;
            }
        }
        if !seg.is_command() {
            return Ok(());
        }
        let program = seg.program.as_str();
        let written = &seg.argv[0];
        if written.contains(['$', '`', '*', '?', '[', '{', '}', '^', '%']) && written != "[" {
            return Err(format!(
                "The program name `{written}` is computed at run time (variable, substitution, \
                 escape or glob), so it cannot be verified. Write the program name literally."
            ));
        }
        for name in seg.wrappers.iter().map(String::as_str).chain([program]) {
            if self.blocked.contains(name) {
                return Err(format!(
                    "`{name}` is blocked by the security policy: privilege escalation, system \
                     administration and destructive disk commands are not allowed."
                ));
            }
        }
        if !self.allowed.is_empty() {
            let checked = seg
                .wrappers
                .iter()
                .map(String::as_str)
                .filter(|w| !TRANSPARENT_WRAPPERS.contains(w))
                .chain([program]);
            for name in checked {
                if !self.allowed.contains(name) && !ALWAYS_ALLOWED.contains(&name) {
                    let list: Vec<&str> = self.allowed.iter().map(String::as_str).collect();
                    return Err(format!(
                        "`{name}` is not in the list of allowed programs for this project ({}).",
                        list.join(", ")
                    ));
                }
            }
        }
        if NAME_DEFINING_BUILTINS.contains(&program) {
            return Err(format!(
                "`{program}` is not allowed: it can make a command name run a different program."
            ));
        }
        if NETWORK_PROGRAMS.contains(&program) && !scope.network {
            return Err(format!(
                "`{program}` needs network access, which is disabled for this agent. Work with \
                 local files instead."
            ));
        }
        let args = seg.args();
        if writes_its_arguments(program, args) {
            for a in args {
                check_git_metadata(program, a)?;
            }
        }
        match program {
            p if REMOVERS.contains(&p) => check_rm(p, args, scope.root),
            "cd" | "pushd" => check_cd(program, args, scope.root),
            "chmod" => check_chmod(args),
            "git" => check_git(args),
            "kill" => check_kill(args),
            "pkill" | "killall" => check_pkill(program, args),
            "eval" => self.validate_depth(&args.join(" "), scope, depth + 1),
            "find" => self.check_find(args, scope, depth),
            "expect" => check_expect(args),
            "dropdb" | "dropuser" => check_dropdb(program, args),
            p if VARIABLE_SETTERS.contains(&p) => check_variable_setters(args),
            p if VARIABLE_READERS.contains(&p) => check_variable_readers(args),
            "printf" => check_printf(args),
            "tee" => check_tee(args, scope.root),
            p if SHELLS.contains(&p) => self.check_shell(p, args, scope, depth),
            p if POWERSHELLS.contains(&p) => self.check_powershell(p, args, scope, depth),
            p if DATABASE_CLIENTS.contains(&p) => check_sql(p, &args.join(" ")),
            _ => Ok(()),
        }
    }

    fn check_shell(&self, shell: &str, args: &[String], scope: Scope<'_>, depth: usize) -> Check {
        let mut i = 0;
        let mut inline = false;
        let mut stdin = false;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--" {
                i += 1;
                break;
            }
            if let Some(cmd) = a
                .strip_prefix("--command=")
                .or_else(|| a.strip_prefix("--commands="))
            {
                return self.validate_depth(cmd, scope, depth + 1);
            }
            if a == "--command" || a == "--commands" {
                inline = true;
                i += 1;
                break;
            }
            if a.starts_with("--") {
                i += 1;
                continue;
            }
            if a.starts_with('-') || a.starts_with('+') {
                if a.starts_with('-') {
                    inline |= a.contains('c');
                    stdin |= a.contains('s');
                }
                // `-o name` / `-O name` (possibly clustered, as in `-euo
                // pipefail`) consume the next word.
                i += if a.contains(['o', 'O']) { 2 } else { 1 };
                continue;
            }
            break;
        }
        if inline {
            return match args.get(i) {
                Some(inner) => self.validate_depth(inner, scope, depth + 1),
                None => Err(format!("`{shell} -c` needs a command string.")),
            };
        }
        if i < args.len() && !stdin {
            return Ok(()); // a script file
        }
        Err(format!(
            "Running `{shell}` without `-c` or a script file would execute commands read from \
             standard input, which cannot be verified. Use `{shell} -c '<command>'` instead."
        ))
    }

    fn check_powershell(
        &self,
        shell: &str,
        args: &[String],
        scope: Scope<'_>,
        depth: usize,
    ) -> Check {
        const VALUE_OPTIONS: &[&str] = &[
            "executionpolicy",
            "workingdirectory",
            "outputformat",
            "inputformat",
            "windowstyle",
            "configurationname",
            "version",
            "settingsfile",
            "custompipename",
        ];
        const VALUE_ALIASES: &[&str] = &["ex", "ep", "wd", "of", "if", "w", "v", "o"];
        let mut i = 0;
        while i < args.len() {
            let a = args[i].to_lowercase();
            if a == "-" {
                break;
            }
            let Some(name) = a.strip_prefix('-').or_else(|| a.strip_prefix('/')) else {
                // First positional: a command for Windows PowerShell, a
                // script file for PowerShell 7.
                return if shell == "powershell" {
                    self.validate_depth(&args[i..].join(" "), scope, depth + 1)
                } else {
                    Ok(())
                };
            };
            if name.starts_with('c') && "command".starts_with(name) {
                return match args.get(i + 1) {
                    Some(_) => self.validate_depth(&args[i + 1..].join(" "), scope, depth + 1),
                    None => Err(format!("`{shell} -Command` needs a command.")),
                };
            }
            if name == "ec" || (name.starts_with('e') && "encodedcommand".starts_with(name)) {
                return Err(format!(
                    "`{shell} -EncodedCommand` cannot be verified; pass the command in clear \
                     with -Command."
                ));
            }
            if name.starts_with('f') && "file".starts_with(name) {
                return Ok(());
            }
            let takes_value = VALUE_ALIASES.contains(&name)
                || (name.len() >= 2 && VALUE_OPTIONS.iter().any(|o| o.starts_with(name)));
            i += if takes_value { 2 } else { 1 };
        }
        Err(format!(
            "Running `{shell}` without -Command or a script file would execute commands read \
             from standard input, which cannot be verified."
        ))
    }

    fn check_find(&self, args: &[String], scope: Scope<'_>, depth: usize) -> Check {
        let starts = find_start_paths(args);
        let mut destructive = args.iter().any(|a| a == "-delete");
        let mut blocks: Vec<&[String]> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                let from = i + 1;
                i = from;
                while i < args.len() && !matches!(args[i].as_str(), ";" | "+") {
                    i += 1;
                }
                if i == from {
                    return Err("`find -exec` needs a command.".into());
                }
                let block = &args[from..i];
                destructive |= REMOVERS.contains(&normalize_program(&block[0]).as_str());
                blocks.push(block);
            }
            i += 1;
        }
        if destructive {
            for start in &starts {
                check_path_target(start, scope.root, Purpose::Remove)?;
            }
        }
        for block in blocks {
            // `{}` stands for the paths found under each start directory.
            for start in &starts {
                let found = format!("{}/f", start.trim_end_matches('/'));
                let command = block
                    .iter()
                    .map(|w| shell_quote(&w.replace("{}", &found)))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.validate_depth(&command, scope, depth + 1)?;
            }
        }
        Ok(())
    }
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Programs that create, modify, copy or link the paths they are given.
const PATH_WRITERS: &[&str] = &[
    "cp", "mv", "tee", "install", "ln", "touch", "chmod", "dd", "rsync", "tar", "unzip",
];

/// Entries of a `.git` directory that control code execution or
/// repository layout.
const GIT_METADATA_ENTRIES: &[&str] = &["hooks", "config", "info", "worktrees", "modules"];

/// Whether `program` writes to paths among its arguments (`sed` only with
/// `-i`).
fn writes_its_arguments(program: &str, args: &[String]) -> bool {
    PATH_WRITERS.contains(&program)
        || (program == "sed"
            && args
                .iter()
                .any(|a| a.starts_with("-i") || a.starts_with("--in-place")))
}

/// Whether `arg` names a `.git` directory itself or its hooks, config,
/// info, worktrees or modules (also inside `--opt=path` / `of=path` forms).
fn touches_git_metadata(arg: &str) -> bool {
    let lower = arg.to_lowercase().replace('\\', "/");
    let parts: Vec<&str> = lower
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    parts.iter().enumerate().any(|(i, part)| {
        let is_git = *part == ".git" || part.ends_with("=.git");
        is_git
            && parts
                .get(i + 1)
                .is_none_or(|next| GIT_METADATA_ENTRIES.contains(next))
    })
}

fn check_git_metadata(program: &str, arg: &str) -> Check {
    if touches_git_metadata(arg) {
        return Err(format!(
            "Refusing to let `{program}` modify `{arg}`: repository metadata (.git hooks, \
             config, info, worktrees and modules) cannot be changed by agents."
        ));
    }
    Ok(())
}

/// Start paths of a `find` command (`.` when none is given), skipping the
/// options that may precede them (`-L`, `-P`, `-H`, `-D opts`, `-O3`, …) and
/// including BSD `-f path`.
fn find_start_paths(args: &[String]) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-H" | "-L" | "-P" | "-E" | "-X" | "-d" | "-s" | "-x" => i += 1,
            "--" => {
                i += 1;
                break;
            }
            "-D" => i += 2,
            "-f" => {
                if let Some(path) = args.get(i + 1) {
                    starts.push(path.as_str());
                }
                i += 2;
            }
            o if o.starts_with("-O") && o.len() > 2 => i += 1,
            _ => break,
        }
    }
    starts.extend(
        args.iter()
            .skip(i)
            .take_while(|a| !(a.starts_with('-') || matches!(a.as_str(), "(" | "!" | ")")))
            .map(String::as_str),
    );
    if starts.is_empty() {
        starts.push(".");
    }
    starts
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

fn is_forbidden_variable(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    FORBIDDEN_VARIABLES.contains(&upper.as_str())
        || FORBIDDEN_VARIABLE_PREFIXES
            .iter()
            .any(|p| upper.starts_with(p))
}

fn check_variable(name: &str) -> Check {
    if is_forbidden_variable(name) {
        return Err(format!(
            "Setting `{name}` is not allowed: it changes which programs run or injects code \
             into them."
        ));
    }
    Ok(())
}

fn check_variable_setters(args: &[String]) -> Check {
    for a in args.iter().filter(|a| !a.starts_with('-')) {
        if let Some((name, _)) = a.split_once('=') {
            check_variable(name.trim_end_matches('+'))?;
        }
    }
    Ok(())
}

/// `tee FILE…` writes (and by default truncates) every file it is given.
fn check_tee(args: &[String], root: Option<&Path>) -> Check {
    for file in args.iter().filter(|a| !a.starts_with('-')) {
        check_path_target(file, root, Purpose::Write)?;
    }
    Ok(())
}

/// `printf -v NAME` assigns the formatted text to `NAME`.
fn check_printf(args: &[String]) -> Check {
    for pair in args.windows(2) {
        if pair[0] == "-v" {
            check_variable(&pair[1])?;
        }
    }
    Ok(())
}

fn check_variable_readers(args: &[String]) -> Check {
    for a in args {
        check_variable(a)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Why a path is being checked, for error messages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Remove,
    Write,
    ChangeDir,
}

impl Purpose {
    fn verb(self) -> &'static str {
        match self {
            Purpose::Remove => "remove",
            Purpose::Write => "write to",
            Purpose::ChangeDir => "change directory to",
        }
    }
}

fn has_drive_prefix(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn is_absolute_like(t: &str) -> bool {
    t.starts_with('/') || t.starts_with('\\') || has_drive_prefix(t) || Path::new(t).is_absolute()
}

fn normalize_lexically(path: &Path) -> PathBuf {
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

/// Canonicalize the longest existing prefix of `path` (resolving symbolic
/// links) and re-append the rest.
fn resolve_lenient(path: &Path) -> PathBuf {
    let normalised = normalize_lexically(path);
    let mut existing = normalised.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.iter().rev() {
        out.push(name);
    }
    out
}

fn is_within(root: &Path, target: &str) -> bool {
    resolve_lenient(Path::new(target)).starts_with(resolve_lenient(root))
}

fn is_same_path(root: &Path, target: &str) -> bool {
    resolve_lenient(Path::new(target)) == resolve_lenient(root)
}

/// Common rules for a path that is removed, written or entered.
fn check_path_target(target: &str, root: Option<&Path>, purpose: Purpose) -> Check {
    let verb = purpose.verb();
    let deny = |why: &str| {
        Err(format!(
            "Refusing to {verb} `{target}`: {why}. Use an exact path inside the workspace."
        ))
    };
    let t = target.trim();
    if t.is_empty() {
        return deny("the path is empty");
    }
    if t.contains(EXPANSION_CHARS) {
        return deny(
            "it contains a shell expansion or wildcard ($, `, *, ?, [, { or ~) whose result \
             cannot be checked",
        );
    }
    if purpose == Purpose::Write && (DEVICE_SINKS.contains(&t) || t.starts_with("/dev/fd/")) {
        return Ok(());
    }
    if has_drive_prefix(t) && t[2..].trim_matches(['\\', '/']).is_empty() {
        return deny("it is the root of a drive");
    }
    if is_absolute_like(t) {
        if let Some(root) = root {
            if !is_within(root, t) {
                return deny("it is outside the workspace");
            }
            return Ok(());
        }
        // Without a workspace root, only well-known system locations are
        // protected.
        let parts: Vec<&str> = t
            .split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        if parts.is_empty() {
            return deny("it is the filesystem root");
        }
        if parts.contains(&"..") {
            return deny("absolute paths containing `..` are ambiguous");
        }
        if parts.len() <= 2 && PROTECTED_ROOTS.contains(&parts[0].to_lowercase().as_str()) {
            return deny("it is a system directory");
        }
        return Ok(());
    }
    let mut level: i64 = 0;
    for part in t.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                level -= 1;
                if level < 0 {
                    return deny("it points outside the working directory");
                }
            }
            _ => level += 1,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rm / cd / xargs rm
// ---------------------------------------------------------------------------

fn check_rm(program: &str, args: &[String], root: Option<&Path>) -> Check {
    let mut options_done = false;
    for a in args {
        if !options_done && a == "--" {
            options_done = true;
            continue;
        }
        if !options_done && a.starts_with('-') && a.len() > 1 {
            if a == "--no-preserve-root" {
                return Err(format!("`{program} --no-preserve-root` is never allowed."));
            }
            continue;
        }
        check_path_target(a, root, Purpose::Remove)?;
        if matches!(a.trim(), "." | "./") {
            return Err(format!(
                "Refusing to remove `{a}`: it would delete everything in the working directory."
            ));
        }
        if let Some(root) = root
            && is_absolute_like(a)
            && is_same_path(root, a)
        {
            return Err(format!(
                "Refusing to remove `{a}`: it is the whole workspace."
            ));
        }
    }
    Ok(())
}

fn check_cd(program: &str, args: &[String], root: Option<&Path>) -> Check {
    let targets: Vec<&String> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "-L" | "-P" | "-e" | "-@" | "--" | "-n"))
        .collect();
    if targets.is_empty() {
        return Err(format!(
            "`{program}` without a directory goes to the home directory, outside the \
             workspace; name the directory explicitly."
        ));
    }
    for t in targets {
        let stack_jump = t == "-"
            || ((t.starts_with('+') || t.starts_with('-'))
                && t.len() > 1
                && t[1..].chars().all(|c| c.is_ascii_digit()));
        if stack_jump {
            return Err(format!(
                "`{program} {t}` jumps to a remembered directory that cannot be checked; name \
                 the directory explicitly."
            ));
        }
        check_path_target(t, root, Purpose::ChangeDir)?;
    }
    Ok(())
}

/// `… | xargs rm`: the removed paths come from the commands feeding the
/// pipeline, so their path arguments must follow the removal rules.
fn check_xargs_removals(segments: &[CommandSegment], root: Option<&Path>) -> Check {
    for (idx, seg) in segments.iter().enumerate() {
        let is_xargs_removal =
            REMOVERS.contains(&seg.program.as_str()) && seg.wrappers.iter().any(|w| w == "xargs");
        if !is_xargs_removal || !seg.piped_input {
            continue;
        }
        let upstream: Vec<&CommandSegment> = segments[..idx]
            .iter()
            .filter(|s| s.pipeline == seg.pipeline && s.is_command())
            .collect();
        if upstream.is_empty() {
            return Err(format!(
                "`xargs {}` would remove paths produced by a command that cannot be inspected \
                 here; list the files explicitly.",
                seg.program
            ));
        }
        for up in upstream {
            let candidates: Vec<&str> = if up.program == "find" {
                find_start_paths(up.args())
            } else {
                up.args()
                    .iter()
                    .filter(|a| !a.starts_with('-'))
                    .map(String::as_str)
                    .collect()
            };
            for candidate in candidates {
                check_path_target(candidate, root, Purpose::Remove)
                    .map_err(|e| format!("{e} (`{}` feeds `xargs {}`)", up.program, seg.program))?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// expect
// ---------------------------------------------------------------------------

fn check_expect(args: &[String]) -> Check {
    let inline = args
        .iter()
        .any(|a| a.starts_with("-c") || a == "-" || a == "-i");
    let has_script = args.iter().any(|a| !a.starts_with('-'));
    if inline || !has_script {
        return Err(
            "`expect` with inline commands (-c) or standard input cannot be verified; \
                    run a script file instead."
                .into(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// chmod
// ---------------------------------------------------------------------------

fn check_chmod(args: &[String]) -> Check {
    for a in args {
        if mode_sets_setid(a) {
            return Err(format!(
                "`chmod {a}` would set the setuid/setgid bit, which is not allowed."
            ));
        }
    }
    Ok(())
}

fn mode_sets_setid(mode: &str) -> bool {
    if mode.len() >= 4 && mode.chars().all(|c| ('0'..='7').contains(&c)) {
        return u32::from_str_radix(mode, 8).is_ok_and(|v| v & 0o6000 != 0);
    }
    if mode.is_empty() || !mode.chars().all(|c| "ugoarwxXst+-=,".contains(c)) {
        return false;
    }
    for clause in mode.split(',') {
        let mut adding = false;
        for c in clause.chars() {
            match c {
                '+' | '=' => adding = true,
                '-' => adding = false,
                's' if adding => return true,
                _ => {}
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

fn check_git(args: &[String]) -> Check {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-c" {
            check_git_config_override(args.get(i + 1).map_or("", String::as_str))?;
            i += 2;
        } else if let Some(kv) = a.strip_prefix("--config-env=") {
            check_git_config_override(kv)?;
            i += 1;
        } else if a == "--config-env" {
            check_git_config_override(args.get(i + 1).map_or("", String::as_str))?;
            i += 2;
        } else if a.starts_with("--exec-path=") {
            return Err(
                "`git --exec-path=<dir>` would run git commands from another directory; \
                        it is not allowed."
                    .into(),
            );
        } else if matches!(a, "-C" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let Some(sub) = args.get(i) else {
        return Ok(());
    };
    let rest = &args[i + 1..];
    let deny = |what: &str| {
        Err(format!(
            "`git {what}` runs arbitrary commands and is not allowed; run the commands \
             directly instead."
        ))
    };
    match sub.as_str() {
        "config" => check_git_config(rest),
        "push" => check_git_push(rest),
        "rebase" => {
            let exec = rest.iter().any(|a| {
                a == "--exec"
                    || a.starts_with("--exec=")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('x'))
            });
            if exec { deny("rebase --exec") } else { Ok(()) }
        }
        "bisect" if rest.first().is_some_and(|a| a == "run") => deny("bisect run"),
        "filter-branch" => deny("filter-branch"),
        "worktree" => {
            let action = rest.iter().find(|a| !a.starts_with('-'));
            if action.is_some_and(|a| a == "list") {
                Ok(())
            } else {
                Err(
                    "Only `git worktree list` is allowed; workspaces are managed by the \
                     framework."
                        .into(),
                )
            }
        }
        "submodule" => {
            let action = rest.iter().find(|a| !a.starts_with('-'));
            if action.is_some_and(|a| a == "foreach") {
                deny("submodule foreach")
            } else {
                Ok(())
            }
        }
        "difftool" | "mergetool" => {
            let custom = rest.iter().any(|a| {
                matches!(a.as_str(), "--extcmd" | "--tool-cmd")
                    || a.starts_with("--extcmd=")
                    || a.starts_with("--tool-cmd=")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('x'))
            });
            if custom {
                deny(&format!("{sub} --extcmd/--tool-cmd"))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Actions accepted as the first argument of `git config`.
const GIT_CONFIG_READ_ACTIONS: &[&str] = &[
    "get",
    "list",
    "--get",
    "--get-all",
    "--get-regexp",
    "-l",
    "--list",
];

/// Flags of `git config` that only affect how or where settings are read.
const GIT_CONFIG_READ_FLAGS: &[&str] = &[
    "--global",
    "--system",
    "--local",
    "--worktree",
    "--includes",
    "--no-includes",
    "--null",
    "-z",
    "--name-only",
    "--show-origin",
    "--show-scope",
    "--all",
    "--regexp",
    "--fixed-value",
    "--bool",
    "--int",
    "--path",
    "--bool-or-int",
    "--expiry-date",
];

/// Options of `git config` that take a separate value and only affect
/// reading.
const GIT_CONFIG_READ_VALUE_OPTIONS: &[&str] = &[
    "--file",
    "-f",
    "--blob",
    "--type",
    "--default",
    "--value",
    "--url",
];

fn is_git_config_read_flag(a: &str) -> bool {
    GIT_CONFIG_READ_FLAGS.contains(&a)
        || [
            "--type=",
            "--file=",
            "--blob=",
            "--default=",
            "--value=",
            "--url=",
        ]
        .iter()
        .any(|p| a.starts_with(p))
}

fn check_git_config(rest: &[String]) -> Check {
    let deny = || {
        Err(
            "`git config` may only read settings: start with `get`, `list`, `--get`, \
             `--get-all`, `--get-regexp` or `--list` and name at most one key. Changing git \
             configuration is not allowed."
                .to_string(),
        )
    };
    let mut i = 0;
    while let Some(a) = rest.get(i) {
        if GIT_CONFIG_READ_VALUE_OPTIONS.contains(&a.as_str()) {
            i += 2;
        } else if is_git_config_read_flag(a) {
            i += 1;
        } else {
            break;
        }
    }
    match rest.get(i) {
        Some(action) if GIT_CONFIG_READ_ACTIONS.contains(&action.as_str()) => {}
        _ => return deny(),
    }
    i += 1;
    let mut positionals = 0;
    while let Some(a) = rest.get(i) {
        if GIT_CONFIG_READ_VALUE_OPTIONS.contains(&a.as_str()) {
            i += 2;
            continue;
        }
        if is_git_config_read_flag(a) {
            i += 1;
            continue;
        }
        if a.starts_with('-') {
            return deny();
        }
        positionals += 1;
        i += 1;
    }
    if positionals > 1 {
        return deny();
    }
    Ok(())
}

fn check_git_config_override(kv: &str) -> Check {
    let key = kv.split('=').next().unwrap_or("").to_lowercase();
    if GIT_FORBIDDEN_CONFIG_PREFIXES
        .iter()
        .any(|p| key.starts_with(p))
    {
        return Err(format!(
            "Overriding the git setting `{key}` with `-c` is not allowed (it could change the \
             commit identity, load other configuration or run arbitrary programs)."
        ));
    }
    Ok(())
}

fn check_git_push(args: &[String]) -> Check {
    let mut force = false;
    let mut delete = false;
    let mut all = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--force" | "--force-with-lease" | "--force-if-includes" => force = true,
            "--delete" | "-d" => delete = true,
            "--all" | "--mirror" => all = true,
            "--repo" | "-o" | "--push-option" | "--receive-pack" | "--exec" => i += 1,
            _ if a.starts_with("--force-with-lease=") => force = true,
            _ if a.starts_with("--") => {}
            _ if a.starts_with('-') => {
                force |= a.contains('f');
                delete |= a.contains('d');
            }
            _ => positional.push(a),
        }
        i += 1;
    }
    let refspecs = positional.get(1..).unwrap_or(&[]);
    for spec in refspecs {
        let plus = spec.starts_with('+');
        let spec = spec.trim_start_matches('+');
        let deleting = spec.starts_with(':');
        let dst = spec.rsplit(':').next().unwrap_or(spec);
        let dst = dst.strip_prefix("refs/heads/").unwrap_or(dst);
        if (force || plus || delete || deleting) && matches!(dst, "main" | "master") {
            return Err(format!(
                "Force-pushing to or deleting the protected branch `{dst}` is not allowed. \
                 Push to a feature branch instead."
            ));
        }
    }
    if force && (refspecs.is_empty() || all) {
        return Err(
            "A force push must name its branch explicitly (and may never target \
                    `main` or `master`)."
                .into(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// kill / pkill / killall
// ---------------------------------------------------------------------------

fn check_kill(args: &[String]) -> Check {
    let mut i = 0;
    if let Some(first) = args.first() {
        match first.as_str() {
            "-l" | "-L" | "--list" => return Ok(()),
            "-s" | "-n" => i = 2,
            "--" => {}
            f if f.starts_with('-') && args.len() > 1 => i = 1,
            _ => {}
        }
    }
    for pid in args.iter().skip(i) {
        if matches!(pid.as_str(), "-1" | "0" | "-0") {
            return Err(format!(
                "`kill {pid}` would signal every process of the session or system; name the \
                 specific process id instead."
            ));
        }
    }
    Ok(())
}

fn check_pkill(program: &str, args: &[String]) -> Check {
    for a in args {
        let lower = a.to_lowercase();
        let hit = lower
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')))
            .find(|tok| SYSTEM_PROCESSES.contains(tok));
        if let Some(name) = hit {
            return Err(format!(
                "`{program}` may not target the system process `{name}`."
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Databases
// ---------------------------------------------------------------------------

static DESTRUCTIVE_SQL: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?i)\bdrop\b", "DROP"),
        (r"(?i)\btruncate\b", "TRUNCATE"),
        (r"(?i)\bflushall\b", "FLUSHALL"),
        (r"(?i)\bflushdb\b", "FLUSHDB"),
        (r"(?i)\bdropdatabase\b|\.drop\s*\(", "a drop operation"),
        (
            r"(?i)\b(deletemany|remove)\s*\(\s*\{\s*\}\s*\)",
            "an unfiltered delete",
        ),
    ]
    .into_iter()
    .map(|(re, what)| (Regex::new(re).expect("static regex"), what))
    .collect()
});

static DELETE_FROM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bdelete\s+from\b").expect("static regex"));
static WHERE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bwhere\b").expect("static regex"));

fn check_sql(client: &str, text: &str) -> Check {
    for (re, what) in DESTRUCTIVE_SQL.iter() {
        if re.is_match(text) {
            return Err(format!(
                "`{client}` with {what} is blocked: destructive database statements are not \
                 allowed."
            ));
        }
    }
    for statement in text.split(';') {
        if DELETE_FROM.is_match(statement) && !WHERE.is_match(statement) {
            return Err(format!(
                "`{client}` with DELETE without a WHERE clause is blocked; add a WHERE clause."
            ));
        }
    }
    Ok(())
}

fn check_dropdb(program: &str, args: &[String]) -> Check {
    const VALUE_OPTIONS: &[&str] = &[
        "-h",
        "--host",
        "-p",
        "--port",
        "-U",
        "--username",
        "--maintenance-db",
    ];
    let mut names = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if VALUE_OPTIONS.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            names.push(a);
        }
        i += 1;
    }
    if names.is_empty() {
        return Err(format!("`{program}` needs an explicit name."));
    }
    for name in names {
        let lower = name.to_lowercase();
        if !DISPOSABLE_NAME_MARKERS.iter().any(|m| lower.contains(m)) {
            return Err(format!(
                "`{program} {name}` is blocked: only disposable names containing one of {} may \
                 be dropped.",
                DISPOSABLE_NAME_MARKERS.join(", ")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Workspace root used by the rooted tests (it need not exist).
    const ROOT: &str = "/Users/me/work/ws";

    fn policy() -> SecurityPolicy {
        SecurityPolicy::new(&SecurityConfig::default())
    }

    fn rooted() -> SecurityPolicy {
        policy().with_workspace_root(ROOT)
    }

    #[track_caller]
    fn allowed(p: &SecurityPolicy, cmd: &str) {
        if let Err(e) = p.validate(cmd) {
            panic!("`{cmd}` should be allowed but was denied: {e}");
        }
    }

    #[track_caller]
    fn denied(p: &SecurityPolicy, cmd: &str) {
        assert!(p.validate(cmd).is_err(), "`{cmd}` should be denied");
    }

    #[track_caller]
    fn check_pairs(p: &SecurityPolicy, deny: &[&str], allow: &[&str]) {
        for cmd in deny {
            denied(p, cmd);
        }
        for cmd in allow {
            allowed(p, cmd);
        }
    }

    // -- baseline ----------------------------------------------------------

    #[test]
    fn ordinary_commands_are_allowed() {
        for p in [policy(), rooted()] {
            for cmd in [
                "cargo test --all",
                "ls -la && echo done",
                "npm run build 2>&1 | tail -n 20",
                "git status; git diff HEAD~1",
                "FOO=1 make -j4",
                "python3 -m pytest tests/",
                "cat <<'EOF' > notes.md\nsudo is mentioned here\nEOF",
                "cargo build 2>/dev/null",
            ] {
                allowed(&p, cmd);
            }
        }
    }

    #[test]
    fn empty_and_unparsable_commands_are_denied() {
        let p = policy();
        denied(&p, "");
        denied(&p, "   ");
        denied(&p, "echo 'oops");
        denied(&p, "echo $(ls");
    }

    #[test]
    fn builtin_blocked_programs() {
        let p = policy();
        for prog in BLOCKED_PROGRAMS {
            denied(&p, &format!("{prog} --help"));
        }
        denied(&p, "ls && sudo rm x");
        denied(&p, "echo $(sudo id)");
        denied(&p, "/usr/bin/sudo ls");
        denied(&p, "pkexec rm -rf x");
        denied(&p, "run0 id");
        denied(&p, "nohup /usr/bin/pkexec id");
        denied(&p, "env FOO=1 nohup sudo ls");
        denied(&p, "(cd /tmp && dd if=/dev/zero of=x)");
    }

    #[test]
    fn dynamic_program_names_are_denied() {
        let p = policy();
        denied(&p, "$CMD -rf x");
        denied(&p, "$(echo rm) x");
        denied(&p, "/bin/r? x");
        denied(&p, "{sudo,ls}");
        // `cmd /C` escapes and variables on Windows.
        denied(&p, "shut^down /s");
        denied(&p, "%COMSPEC% /c dir");
        allowed(&p, "[ -f x ] && echo yes");
    }

    #[test]
    fn configured_blocklist_applies_to_wrappers_too() {
        let cfg = SecurityConfig {
            blocked_commands: vec!["docker".into(), "nohup".into()],
            ..SecurityConfig::default()
        };
        let p = SecurityPolicy::new(&cfg);
        denied(&p, "docker ps");
        denied(&p, "nohup make");
        allowed(&p, "podman ps");
        allowed(&p, "make");
    }

    #[test]
    fn allowlist_restricts_programs_and_wrappers() {
        let cfg = SecurityConfig {
            allowed_commands: vec!["cargo".into(), "git".into()],
            ..SecurityConfig::default()
        };
        let p = SecurityPolicy::new(&cfg);
        allowed(&p, "cargo build && git status");
        allowed(&p, "cd sub && cargo test && echo ok");
        allowed(&p, "exec cargo test");
        denied(&p, "npm install");
        denied(&p, "cargo build | tee log");
        denied(&p, "nohup cargo build");
    }

    #[test]
    fn network_programs_need_permission() {
        let p = policy();
        denied(&p, "curl https://example.com");
        denied(&p, "wget -q https://example.com");
        denied(&p, "echo $(curl -s x)");
        assert!(
            p.validate_with_network("curl https://example.com", true)
                .is_ok()
        );
        let cfg = SecurityConfig {
            allow_network: true,
            ..SecurityConfig::default()
        };
        let p = SecurityPolicy::new(&cfg);
        assert!(p.allows_network());
        allowed(&p, "curl -fsSL https://example.com -o f");
    }

    #[test]
    fn workspace_root_accessors() {
        assert_eq!(policy().workspace_root(), None);
        assert_eq!(rooted().workspace_root(), Some(Path::new(ROOT)));
        // An explicit root overrides the policy's own.
        let p = policy();
        assert!(p.validate("rm -rf /tmp/x").is_ok());
        assert!(
            p.validate_command("rm -rf /tmp/x", false, Some(Path::new(ROOT)))
                .is_err()
        );
    }

    // -- 1. rm targets -----------------------------------------------------

    #[test]
    fn rm_legacy_rules() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "rm -rf /",
                "rm -rf / --no-preserve-root",
                "rm --no-preserve-root -rf x",
                "rm -rf ~",
                "rm -rf ~/projects",
                "rm -rf $HOME",
                "rm -rf *",
                "rm -rf /*",
                "rm -rf ..",
                "rm -rf ../sibling",
                "rm -rf a/../../x",
                "rm -rf .",
                "rm -rf /home",
                "rm -rf /usr/lib",
                "rm -rf /etc/",
                "rm -r /var/log",
                "rm -rf /opt",
                "rm -rf /bin",
                "rm -rf /lib/x",
                "rm -- -rf /",
                "rm -rf C:\\",
            ],
            &[
                "rm -rf target",
                "rm -f src/old.rs",
                "rm -rf ./build/",
                "rm -rf a/../b",
                "rm -rf /tmp/vibe-scratch",
                "rm -rf /usr/local/share/vibe-test",
            ],
        );
    }

    #[test]
    fn rm_expansions_anywhere_are_denied() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "rm -rf ${HOME%/}",
                "rm -rf \"$(echo ~)\"",
                "rm -rf \"$PWD/..\"",
                "rm -rf /U*",
                "rm -rf /*/x",
                "rm *.log",
                "rm -rf build/`whoami`",
                "rm -rf dist/[ab]",
                "rm -rf out/{a,b}",
                "rm -f file?.txt",
                "rmdir ~/x",
                "unlink $HOME/x",
            ],
            &[
                "rm -rf build",
                "rm -rf \"dist\"",
                "rm -rf ./out",
                "rm build.log",
                "rmdir empty-dir",
            ],
        );
    }

    #[test]
    fn rm_absolute_targets_must_be_inside_the_workspace() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "rm -rf /Users/me/.ssh",
                "rm -rf ~/.ssh",
                "rm -rf /tmp/x",
                "rm -rf /Users/me/work/ws/../other",
                "rm -rf /Users/me/work/ws",
                "rm -rf /Users/me/work/ws/",
                "rm -rf /Users/me/work/wsx",
            ],
            &[
                "rm -rf /Users/me/work/ws/target",
                "rm -f /Users/me/work/ws/src/old.rs",
            ],
        );
    }

    #[cfg(unix)]
    #[test]
    fn rm_through_a_symlink_leaving_the_workspace_is_denied() {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = ws.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        std::fs::create_dir(root.join("target")).unwrap();
        let p = policy().with_workspace_root(&root);
        denied(&p, &format!("rm -rf {}/link/data", root.display()));
        allowed(&p, &format!("rm -rf {}/target", root.display()));
    }

    #[test]
    fn find_delete_and_exec_rm_follow_rm_rules() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "find ~ -name '*.tmp' -delete",
                "find / -name x -delete",
                "find /Users/me -exec rm {} +",
                "find $HOME -type f -exec rm -f {} \\;",
                "find .. -delete",
                "find /tmp -execdir rmdir {} +",
                "find . -name x -exec sudo rm {} \\;",
                "find / -maxdepth 0 -exec rm -rf / \\;",
            ],
            &[
                "find . -name '*.o' -delete",
                "find target -exec rm {} +",
                "find /Users/me/work/ws/build -delete",
                "find . -name '*.tmp' -exec rm {} +",
                "find / -name '*.rs'",
                "find ~ -name notes.md",
            ],
        );
    }

    #[test]
    fn xargs_rm_checks_what_feeds_it() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "echo ~ | xargs rm",
                "ls / | xargs rm -rf",
                "find / -name x | xargs rm",
                "find ~ -print0 | xargs -0 rm -f",
                "printf '%s\\n' /etc/hosts | xargs rm",
                "(cat list.txt) | xargs rm",
                "ls | xargs sudo rm",
                "ls | xargs -I{} rm {}",
            ],
            &[
                "find . -name '*.o' | xargs rm",
                "git ls-files -z --deleted | xargs -0 rm -f",
                "cat obsolete.txt | xargs rm",
                "find . -type f | xargs -n 1 wc -l",
                "xargs rm < list.txt",
            ],
        );
    }

    #[test]
    fn writing_redirections_follow_path_rules() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "echo x > ~/x",
                ": > ~/.bashrc",
                "> ~/.profile",
                "echo x >> $HOME/.profile",
                "echo x > /etc/hosts",
                "echo x > /tmp/out",
                "echo x > ../outside.txt",
                "echo x &> ~/log",
                "echo x >| \"$HOME/f\"",
                "(ls) > ~/x",
                "{ ls; } > ~/x",
                "echo x 2> /Users/me/.zshrc",
            ],
            &[
                "echo x > out.txt",
                "echo x >> logs/app.log",
                "cargo test 2>/dev/null",
                "cargo test > /Users/me/work/ws/log.txt 2>&1",
                "echo x >&2",
                "cat < /etc/hosts",
                "echo x > /dev/stderr",
            ],
        );
        // Without a workspace root only system locations are protected.
        let p = policy();
        check_pairs(
            &p,
            &["echo x > /etc/hosts", "echo x > ~/x"],
            &["echo x > /tmp/out"],
        );
    }

    #[test]
    fn cd_is_confined_like_removals() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "cd ~ && rm -rf x",
                "cd && rm x",
                "cd / && ls",
                "cd - && ls",
                "cd /Users/me && rm -rf .ssh",
                "cd .. && ls",
                "cd $HOME",
                "pushd /etc",
            ],
            &[
                "cd sub && make",
                "cd /Users/me/work/ws/sub && make",
                "cd sub/.. && ls",
                "pushd crates/core",
            ],
        );
    }

    // -- 2. git ------------------------------------------------------------

    #[test]
    fn git_config_is_read_only() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "git config core.hooksPath get",
                "git config core.hookspath list",
                "git config user.email a@b.c",
                "git config --global core.editor vim",
                "git config --get a.b c.d",
                "git config -l --unset x",
                "git config --show-origin core.hooksPath /tmp/hooks",
                "git config --add core.pager sh",
                "git config set core.pager sh",
                "git config",
            ],
            &[
                "git config --get core.hooksPath",
                "git config get user.name",
                "git config --list",
                "git config -l",
                "git config --show-origin --list",
                "git config --global --get user.email",
                "git config --get-all remote.origin.fetch",
                "git config --get-regexp '^alias'",
                "git config list --show-scope",
            ],
        );
    }

    #[test]
    fn git_c_overrides() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "git -c user.name=x commit -m m",
                "git -c author.email=x commit",
                "git -c committer.name=x commit",
                "git -c core.sshCommand='sh -c id' fetch",
                "git -c include.path=/tmp/evil status",
                "git -c includeIf.gitdir:/x.path=/tmp/evil status",
                "git -c CORE.HOOKSPATH=/tmp status",
                "git --config-env=core.pager=EVIL log",
                "git --exec-path=/tmp/evil status",
                "git -c core.askpass=/tmp/x fetch",
                "git -c gpg.program=/tmp/x commit -S",
                "git -c filter.lfs.smudge='sh -c id' checkout .",
                "git -c diff.tool=x difftool",
                "git -c merge.tool=x mergetool",
                "git -c protocol.ext.allow=always fetch",
                "git -c uploadpack.packObjectsHook=/tmp/x fetch",
                "git -c receive.denyCurrentBranch=ignore push",
            ],
            &[
                "git -c color.ui=never status",
                "git -c core.quotepath=off status",
                "git -C sub log --oneline",
                "git --exec-path",
            ],
        );
    }

    #[test]
    fn git_environment_overrides_are_denied() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.pager GIT_CONFIG_VALUE_0=sh git log",
                "GIT_CONFIG_KEY_0=core.pager git log",
                "GIT_CONFIG_VALUE_0=sh git log",
                "GIT_CONFIG_PARAMETERS=\"'core.pager'='sh'\" git log",
                "GIT_DIR=/tmp/other git status",
                "GIT_WORK_TREE=/ git checkout .",
                "GIT_SSH_COMMAND='sh -c id' git fetch",
                "GIT_EXTERNAL_DIFF=/tmp/x git diff",
                "GIT_EDITOR=/tmp/x git commit",
                "GIT_PAGER='sh -c id' git log",
                "GIT_AUTHOR_NAME=x git commit -m m",
                "GIT_AUTHOR_DATE=2020-01-01 git commit -m m",
                "GIT_COMMITTER_EMAIL=x@y git commit -m m",
                "GIT_EXEC_PATH=/tmp/x git status",
                "env GIT_SSH_COMMAND=x git fetch",
                "export GIT_DIR=/tmp/x",
            ],
            &[
                "GIT_TRACE=1 git status",
                "env GIT_TERMINAL_PROMPT=0 git fetch",
            ],
        );
    }

    #[test]
    fn git_subcommands_that_run_programs() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "git rebase -x 'make test' main",
                "git rebase --exec 'make' main",
                "git rebase --exec=make main",
                "git rebase -ix make main",
                "git bisect run make test",
                "git filter-branch --tree-filter 'rm x' HEAD",
                "git submodule foreach 'rm -rf x'",
                "git submodule --quiet foreach 'ls'",
                "git difftool -x 'sh -c id'",
                "git difftool --extcmd=vimdiff",
                "git mergetool --tool-cmd='sh -c id'",
                "git difftool -yx 'sh -c id'",
            ],
            &[
                "git rebase main",
                "git rebase -i main",
                "git rebase -X theirs main",
                "git bisect start",
                "git bisect good HEAD~3",
                "git submodule update --init",
                "git difftool --tool=vimdiff",
                "git difftool -y",
                "git mergetool",
            ],
        );
    }

    #[test]
    fn git_push_rules() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "git push --force origin main",
                "git push -f origin master",
                "git push origin +main",
                "git push --force-with-lease origin HEAD:refs/heads/main",
                "git push origin --delete main",
                "git push origin :master",
                "git push --force",
                "git push -fu origin",
            ],
            &[
                "git push origin feature/x",
                "git push --force origin vibe/task-1",
                "git push -u origin HEAD",
                "git commit -m 'fix: thing'",
            ],
        );
    }

    // -- 3. wrappers, shells and name tricks -------------------------------

    #[test]
    fn wrappers_cannot_hide_blocked_programs() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "setsid sudo id",
                "stdbuf -oL sudo id",
                "caffeinate -i sudo id",
                "chronic sudo id",
                "unbuffer sudo id",
                "ionice -c 3 sudo id",
                "taskset 0x1 sudo id",
                "flock /tmp/lock sudo id",
                "flock /tmp/lock -c 'sudo id'",
                "watch -n 1 sudo id",
                "watch -x sudo id",
                "strace -f -o log sudo id",
                "ltrace sudo id",
                "script -q -c 'sudo id' /dev/null",
                "script -q out.log sudo id",
                "script -t 0 out.log sudo id",
                "busybox reboot",
                "toybox reboot",
                "busybox sh -c 'sudo id'",
                "doas ls",
                "nohup setsid stdbuf -o0 sudo id",
                "setsid rm -rf /",
            ],
            &[
                "setsid make",
                "stdbuf -oL cargo test",
                "caffeinate -i cargo build",
                "chronic cargo test",
                "unbuffer cargo test",
                "ionice -c3 make",
                "taskset -c 0 make",
                "flock /tmp/lock cargo build",
                "flock /tmp/lock -c 'cargo build'",
                "watch -n 5 git status",
                "strace -f ls",
                "ltrace ls",
                "script -q -c 'make' /dev/null",
                "busybox ls",
                "toybox ls",
            ],
        );
    }

    #[test]
    fn other_shells_are_validated_recursively() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "bash -c 'sudo ls'",
                "sh -c \"rm -rf /\"",
                "zsh -lc 'echo ok && reboot'",
                "bash -c \"bash -c 'sudo id'\"",
                "fish --command='sudo id'",
                "tcsh -c 'sudo id'",
                "csh -c 'sudo id'",
                "rc -c 'sudo id'",
                "elvish -c 'sudo id'",
                "nu -c 'sudo id'",
                "nu --commands 'sudo id'",
                "pwsh -c 'sudo id'",
                "pwsh -Command 'shutdown /s'",
                "powershell -Command 'shutdown /s'",
                "powershell -command sudo id",
                "powershell -NoProfile -ExecutionPolicy Bypass -c 'shutdown'",
                "powershell 'shutdown /s'",
                "pwsh -EncodedCommand ZQBjAGgAbwA=",
                "pwsh -e ZQBjAGgAbwA=",
                "pwsh",
                "pwsh -",
                "echo 'sudo id' | sh",
                "echo 'sudo id' | tcsh",
                "bash -s < script.sh",
                "bash -c",
                "bash -euo pipefail -c 'sudo id'",
                "bash -o errexit -c 'sudo id'",
            ],
            &[
                "bash -c 'cargo test && echo ok'",
                "sh ./scripts/build.sh --release",
                "bash -euo pipefail -c 'make'",
                "tcsh -c 'make'",
                "csh -c 'make'",
                "rc -c 'make'",
                "elvish -c 'make'",
                "nu -c 'ls'",
                "pwsh -File build.ps1",
                "pwsh -NoProfile -c 'Get-ChildItem'",
                "pwsh build.ps1",
                "powershell -ExecutionPolicy Bypass -File build.ps1",
            ],
        );
    }

    #[test]
    fn name_defining_builtins_are_denied() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "alias ls='rm -rf /'",
                "alias",
                "hash -p /bin/sh ls",
                "hash -r",
                "enable -f ./evil.so evil",
                "enable -n cd",
            ],
            &["unalias ll", "command -v cargo", "type cargo"],
        );
    }

    #[test]
    fn xargs_must_name_its_program() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "xargs",
                "ls | xargs",
                "ls | xargs -0",
                "xargs -I CMD CMD x",
                "xargs -ICMD CMD",
                "xargs -i {} x",
                "xargs --process-slot-var=SLOT sh -c 'echo $SLOT'",
                "eval 'sudo id'",
            ],
            &[
                "ls | xargs -n1 echo",
                "ls | xargs -I{} cp {} dst/",
                "ls | xargs wc -l",
            ],
        );
    }

    #[test]
    fn expect_cannot_run_inline_tcl() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "expect -c 'spawn sudo id'",
                "expect",
                "expect -",
                "expect -i",
            ],
            &["expect run.exp", "expect -f run.exp"],
        );
    }

    // -- 4. environment ----------------------------------------------------

    #[test]
    fn dangerous_variables_cannot_be_set() {
        let p = policy();
        for var in [
            "PATH",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_LIBRARY_PATH",
            "IFS",
            "BASH_ENV",
            "ENV",
            "PROMPT_COMMAND",
            "PERL5OPT",
            "PYTHONSTARTUP",
            "NODE_OPTIONS",
            "RUBYOPT",
        ] {
            denied(&p, &format!("{var}=/tmp/x ls"));
            denied(&p, &format!("env {var}=/tmp/x ls"));
            denied(&p, &format!("env -i '{var}=/tmp/x' ls"));
            denied(&p, &format!("nohup env {var}=/tmp/x ls"));
            denied(&p, &format!("export {var}=/tmp/x"));
            denied(&p, &format!("declare -x {var}=/tmp/x"));
            denied(&p, &format!("{var}=/tmp/x"));
            denied(&p, &format!("{var}+=:/tmp/x; ls"));
        }
        check_pairs(
            &p,
            &[
                "path=/tmp/x ls",
                "for PATH in /tmp; do ls; done",
                "read PATH < f",
                "printf -v PATH '%s' /tmp",
                "readonly LD_PRELOAD=/x",
                "local IFS=x",
            ],
            &[
                "RUST_LOG=debug cargo run",
                "env CI=1 npm test",
                "env -u PATH ls",
                "export FOO=1",
                "export PATH",
                "echo $PATH",
                "read -r line < f",
                "printf '%s\\n' PATH",
                "for f in a b; do echo $f; done",
            ],
        );
    }

    // -- unchanged validators ----------------------------------------------

    #[test]
    fn chmod_validator() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "chmod 4755 x",
                "chmod 2755 x",
                "chmod 6755 x",
                "chmod 04755 x",
                "chmod u+s x",
                "chmod g+s x",
                "chmod +s x",
                "chmod -R a=rwxs dir",
            ],
            &[
                "chmod 755 x",
                "chmod +x script.sh",
                "chmod -R u+rw dir",
                "chmod u-s x",
                "chmod 0755 x",
            ],
        );
    }

    #[test]
    fn kill_validator() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "kill -9 -1",
                "kill 0",
                "kill -- -1",
                "kill -TERM -0",
                "kill -1",
            ],
            &[
                "kill 1234",
                "kill -9 1234",
                "kill -s TERM 1234",
                "kill -l",
                "kill -0 1234",
            ],
        );
    }

    #[test]
    fn pkill_validator() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "pkill systemd",
                "killall -9 launchd",
                "pkill -f 'sshd: user'",
                "killall WindowServer",
                "killall explorer.exe",
                "pkill dockerd",
            ],
            &["pkill -f 'node server.js'", "killall my-dev-server"],
        );
    }

    #[test]
    fn database_clients() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "psql -c 'DROP TABLE users'",
                "psql -c 'drop database app'",
                "mysql -e 'TRUNCATE orders'",
                "psql -c 'DELETE FROM users'",
                "psql -c 'DELETE FROM a WHERE id = 1; DELETE FROM b'",
                "redis-cli FLUSHALL",
                "redis-cli flushdb",
                "mongosh --eval 'db.dropDatabase()'",
                "mongosh --eval 'db.users.drop()'",
                "mongosh --eval 'db.users.deleteMany({})'",
            ],
            &[
                "psql -c 'SELECT * FROM users'",
                "psql -c 'DELETE FROM users WHERE id = 3'",
                "redis-cli GET key",
                "mongosh --eval 'db.users.find()'",
                "psql -f migrations/001.sql",
            ],
        );
    }

    #[test]
    fn dropdb_only_for_disposable_names() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "dropdb production",
                "dropuser admin",
                "dropdb -h localhost -U postgres",
            ],
            &[
                "dropdb app_test",
                "dropdb -h localhost -U postgres myapp_dev",
                "dropuser sandbox_user",
            ],
        );
    }

    // -- review follow-up ---------------------------------------------------

    #[test]
    fn wrapper_value_options_do_not_hide_programs() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "env -P /usr/bin sudo id",
                "env -C / rm -rf etc",
                "env --chdir=/Users/me rm -rf .ssh",
                "env -C ~ ls",
                "xargs --max-args 1 sudo id",
                "xargs --max-procs 2 sudo id",
                "xargs --max-chars 10 sudo id",
                "xargs --arg-file list.txt sudo id",
                "xargs --delimiter , sudo id",
                "stdbuf --output L sudo id",
                "stdbuf --input 0 sudo id",
                "stdbuf --error 0 sudo id",
                "time -o out.txt sudo id",
                "time -f %e sudo id",
                "/usr/bin/time --output out.txt sudo id",
                "/usr/bin/time --format %e sudo id",
                "time -o ~/.bashrc make",
                "strace --output log sudo id",
                "strace --attach 1 --user root sudo id",
                "strace -o ~/.profile ls",
                "ltrace --output log sudo id",
                "nice --adjustment 5 sudo id",
                "timeout --signal KILL 5 sudo id",
                "ionice --class 3 sudo id",
                "caffeinate -w 42 sudo id",
                "watch --interval 1 sudo id",
                "script -q ~/.bashrc make",
                "script -q -c make /etc/motd",
            ],
            &[
                "env -C sub make",
                "env --chdir=/Users/me/work/ws/sub make",
                "xargs --max-args 1 echo",
                "stdbuf --output L cargo test",
                "time -o timing.txt make",
                "strace -o trace.log ls",
                "script -q session.log make",
                "watch --interval 5 git status",
            ],
        );
    }

    #[test]
    fn find_leading_options_do_not_hide_start_paths() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "find -L / -delete",
                "find -P ~ -exec rm {} +",
                "find -H -L /Users/me -delete",
                "find -D tree / -delete",
                "find -O3 / -delete",
                "find -f / -delete",
                "find -L ~ -print0 | xargs -0 rm",
            ],
            &[
                "find -L . -name '*.o' -delete",
                "find -P target -exec rm {} +",
                "find -L . -name '*.o' | xargs rm",
            ],
        );
    }

    #[test]
    fn cdpath_cannot_redirect_cd() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "CDPATH=/; cd etc && rm -rf hosts",
                "export CDPATH=/",
                "CDPATH=/ cd etc",
            ],
            &["cd etc && ls"],
        );
    }

    #[test]
    fn tee_follows_write_rules() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "echo x | tee ~/.bashrc",
                "echo x | tee -a /etc/hosts",
                "echo x | tee out.txt $HOME/x",
                "echo x | tee ../outside.txt",
            ],
            &[
                "cargo test | tee test.log",
                "echo x | tee -a logs/a.log /dev/null",
            ],
        );
    }

    #[test]
    fn dev_tcp_redirections_need_network() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "cat < /dev/tcp/example.com/80",
                "echo x > /dev/udp/192.0.2.1/53",
                "exec 3<>/dev/tcp/example.com/80",
            ],
            &["cat < input.txt"],
        );
        assert!(
            p.validate_with_network("cat < /dev/tcp/example.com/80", true)
                .is_ok()
        );
    }

    // -- addendum ------------------------------------------------------------

    #[test]
    fn more_shells_are_validated() {
        let p = policy();
        check_pairs(
            &p,
            &[
                "ash -c 'sudo id'",
                "mksh -c 'sudo id'",
                "busybox ash -c 'sudo id'",
                "busybox sh -c 'rm -rf /'",
                "echo 'sudo id' | busybox sh",
                "echo 'sudo id' | mksh",
            ],
            &["ash -c 'make'", "mksh -c 'make'", "busybox sh -c 'ls'"],
        );
    }

    #[test]
    fn git_metadata_cannot_be_written() {
        let p = rooted();
        check_pairs(
            &p,
            &[
                "cp x .git/hooks/pre-commit",
                "echo x | tee .git/hooks/pre-commit",
                "chmod +x .git/hooks/pre-commit",
                "mv evil .git/config",
                "ln -s ../../evil.sh .git/hooks/post-checkout",
                "install -m 755 hook .git/hooks/pre-push",
                "touch .git/hooks/x",
                "sed -i s/a/b/ .git/config",
                "sed --in-place=.bak s/a/b/ .git/config",
                "rsync -a hooks/ .git/hooks/",
                "tar -xf hooks.tar -C .git",
                "unzip hooks.zip -d .git/hooks",
                "dd if=x of=.git/hooks/pre-commit",
                "cp -r evil .git/",
                "cp x sub/.git/info/attributes",
                "cp x .GIT/hooks/pre-commit",
                "cp x .git//hooks/y",
                "cp x ./.git/./hooks/y",
                "cp x --target-directory=.git/hooks",
                "cp x ../../../.git/hooks/pre-commit",
                "cp -r .git/modules/x .git/worktrees/y",
                "echo x > .git/hooks/pre-commit",
                "echo x >> .git/config",
                "cat evil > sub/.git/info/exclude",
                "git worktree add ../elsewhere",
                "git worktree remove x",
                "git worktree prune",
            ],
            &[
                "cp x .github/workflows/ci.yml",
                "cat .git/config",
                "grep -r x .git/hooks",
                "ls .git",
                "echo x > .gitignore",
                "touch .gitkeep",
                "sed s/a/b/ .git/config",
                "cp README.md docs/",
                "git worktree list",
                "git status",
            ],
        );
    }
}
