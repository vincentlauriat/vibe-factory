//! Denylist-based validation of shell commands.
//!
//! Every simple command found by the parser (see `parse_command`) is checked
//! in turn: dynamic program names, the built-in and configured blocked
//! programs, the optional allowlist, network gating, and finally a
//! per-program validator for commands that are useful but dangerous with some
//! arguments (`rm`, `chmod`, `git`, `kill`, shells, database clients, …).
//!
//! This is a best-effort guard against destructive mistakes, not a sandbox:
//! interpreters such as `python -c` can still do anything the operating
//! system allows. Run agents in an isolated workspace.

use std::collections::BTreeSet;
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
];

/// Programs that reach the network and therefore require network access.
pub const NETWORK_PROGRAMS: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "telnet", "ftp", "sftp", "scp", "ssh",
];

/// Shells whose `-c` argument is validated recursively.
pub const SHELLS: &[&str] = &["bash", "sh", "zsh", "fish", "dash", "ksh"];

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

/// Top-level directories `rm` may not target, nor their direct children.
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

/// `git -c` keys that could impersonate an author or run arbitrary programs.
const GIT_FORBIDDEN_CONFIG_PREFIXES: &[&str] = &[
    "user.",
    "author.",
    "committer.",
    "alias.",
    "core.sshcommand",
    "core.pager",
    "core.editor",
    "core.hookspath",
    "core.fsmonitor",
    "core.gitproxy",
    "sequence.editor",
    "credential.helper",
    "diff.external",
];

type Check = Result<(), String>;

/// Validates shell commands before they are executed.
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    blocked: BTreeSet<String>,
    allowed: BTreeSet<String>,
    allow_network: bool,
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
        }
    }

    /// Whether the configuration itself grants network access.
    #[must_use]
    pub fn allows_network(&self) -> bool {
        self.allow_network
    }

    /// Check `command` using the configuration's network setting.
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
        if command.trim().is_empty() {
            return Err("The command is empty.".into());
        }
        self.validate_depth(command, self.allow_network || network_permitted, 0)
    }

    fn validate_depth(&self, command: &str, network: bool, depth: usize) -> Check {
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
            self.validate_segment(segment, network, depth)?;
        }
        Ok(())
    }

    fn validate_segment(&self, seg: &CommandSegment, network: bool, depth: usize) -> Check {
        let program = seg.program.as_str();
        let written = &seg.argv[0];
        if written.contains(['$', '`', '*', '?', '[', '{', '}', '^', '%']) && written != "[" {
            return Err(format!(
                "The program name `{written}` is computed at run time (variable, substitution, \
                 escape or glob), so it cannot be verified. Write the program name literally."
            ));
        }
        if self.blocked.contains(program) {
            return Err(format!(
                "`{program}` is blocked by the security policy: privilege escalation, system \
                 administration and destructive disk commands are not allowed."
            ));
        }
        if !self.allowed.is_empty()
            && !self.allowed.contains(program)
            && !ALWAYS_ALLOWED.contains(&program)
        {
            let list: Vec<&str> = self.allowed.iter().map(String::as_str).collect();
            return Err(format!(
                "`{program}` is not in the list of allowed programs for this project ({}).",
                list.join(", ")
            ));
        }
        if NETWORK_PROGRAMS.contains(&program) && !network {
            return Err(format!(
                "`{program}` needs network access, which is disabled for this agent. Work with \
                 local files instead."
            ));
        }
        let args = seg.args();
        match program {
            "rm" => check_rm(args),
            "chmod" => check_chmod(args),
            "git" => check_git(args),
            "kill" => check_kill(args),
            "pkill" | "killall" => check_pkill(program, args),
            "eval" => self.validate_depth(&args.join(" "), network, depth + 1),
            "find" => self.check_find(args, network, depth),
            "dropdb" | "dropuser" => check_dropdb(program, args),
            p if SHELLS.contains(&p) => self.check_shell(p, args, network, depth),
            p if DATABASE_CLIENTS.contains(&p) => check_sql(p, &args.join(" ")),
            _ => Ok(()),
        }
    }

    fn check_shell(&self, shell: &str, args: &[String], network: bool, depth: usize) -> Check {
        let mut i = 0;
        let mut inline = false;
        let mut stdin = false;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--" {
                i += 1;
                break;
            }
            if let Some(cmd) = a.strip_prefix("--command=") {
                return self.validate_depth(cmd, network, depth + 1);
            }
            if a == "--command" {
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
                Some(inner) => self.validate_depth(inner, network, depth + 1),
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

    fn check_find(&self, args: &[String], network: bool, depth: usize) -> Check {
        let mut i = 0;
        while i < args.len() {
            if matches!(args[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                let mut inner = Vec::new();
                i += 1;
                while i < args.len() && !matches!(args[i].as_str(), ";" | "+") {
                    inner.push(shell_quote(&args[i]));
                    i += 1;
                }
                if inner.is_empty() {
                    return Err("`find -exec` needs a command.".into());
                }
                self.validate_depth(&inner.join(" "), network, depth + 1)?;
            }
            i += 1;
        }
        Ok(())
    }
}

fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// rm
// ---------------------------------------------------------------------------

fn check_rm(args: &[String]) -> Check {
    let mut options_done = false;
    for a in args {
        if !options_done && a == "--" {
            options_done = true;
            continue;
        }
        if !options_done && a.starts_with('-') && a.len() > 1 {
            if a == "--no-preserve-root" {
                return Err("`rm --no-preserve-root` is never allowed.".into());
            }
            continue;
        }
        check_rm_target(a)?;
    }
    Ok(())
}

fn check_rm_target(target: &str) -> Check {
    let deny = |why: &str| {
        Err(format!(
            "Refusing to run `rm` on `{target}`: {why}. Remove specific files inside the \
             workspace instead."
        ))
    };
    let t = target.trim();
    if t.is_empty() {
        return deny("the target is empty");
    }
    if matches!(t, "*" | "/*" | "." | "./" | ".*" | "./*") {
        return deny("it would delete everything in the directory");
    }
    if t.starts_with('~') || t.starts_with("$HOME") || t.starts_with("${HOME}") {
        return deny("it points into the home directory");
    }
    let is_windows_drive = {
        let b = t.as_bytes();
        b.len() >= 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && t[2..].trim_matches(['\\', '/']).is_empty()
    };
    if is_windows_drive {
        return deny("it is the root of a drive");
    }
    if t.starts_with('/') {
        let parts: Vec<&str> = t
            .split('/')
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
    // Relative path: must not climb out of the working directory.
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
    match sub.as_str() {
        "config" => {
            let read_only = rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--get"
                        | "--get-all"
                        | "--get-regexp"
                        | "--get-urlmatch"
                        | "--list"
                        | "-l"
                        | "get"
                        | "list"
                )
            });
            if read_only {
                Ok(())
            } else {
                Err(
                    "`git config` may only be used to read settings (`--get`, `--list`); \
                     changing git configuration is not allowed."
                        .into(),
                )
            }
        }
        "push" => check_git_push(rest),
        _ => Ok(()),
    }
}

fn check_git_config_override(kv: &str) -> Check {
    let key = kv.split('=').next().unwrap_or("").to_lowercase();
    if GIT_FORBIDDEN_CONFIG_PREFIXES
        .iter()
        .any(|p| key.starts_with(p))
    {
        return Err(format!(
            "Overriding the git setting `{key}` with `-c` is not allowed (it could change the \
             commit identity or run arbitrary programs)."
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

    fn policy() -> SecurityPolicy {
        SecurityPolicy::new(&SecurityConfig::default())
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

    #[test]
    fn ordinary_commands_are_allowed() {
        let p = policy();
        for cmd in [
            "cargo test --all",
            "ls -la && echo done",
            "npm run build 2>&1 | tail -n 20",
            "git status; git diff HEAD~1",
            "FOO=1 make -j4",
            "python3 -m pytest tests/",
            "cat <<'EOF' > notes.md\nsudo is mentioned here\nEOF",
        ] {
            allowed(&p, cmd);
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
    fn configured_blocklist() {
        let cfg = SecurityConfig {
            blocked_commands: vec!["docker".into()],
            ..SecurityConfig::default()
        };
        let p = SecurityPolicy::new(&cfg);
        denied(&p, "docker ps");
        allowed(&p, "podman ps");
    }

    #[test]
    fn allowlist_restricts_programs() {
        let cfg = SecurityConfig {
            allowed_commands: vec!["cargo".into(), "git".into()],
            ..SecurityConfig::default()
        };
        let p = SecurityPolicy::new(&cfg);
        allowed(&p, "cargo build && git status");
        allowed(&p, "cd sub && cargo test && echo ok");
        denied(&p, "npm install");
        denied(&p, "cargo build | tee log");
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
    fn rm_validator() {
        let p = policy();
        for cmd in [
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
        ] {
            denied(&p, cmd);
        }
        for cmd in [
            "rm -rf target",
            "rm -f src/old.rs",
            "rm -rf ./build/",
            "rm -rf a/../b",
            "rm -rf /tmp/vibe-scratch",
            "rm -rf /usr/local/share/vibe-test",
            "rm *.log",
        ] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn chmod_validator() {
        let p = policy();
        for cmd in [
            "chmod 4755 x",
            "chmod 2755 x",
            "chmod 6755 x",
            "chmod 04755 x",
            "chmod u+s x",
            "chmod g+s x",
            "chmod +s x",
            "chmod -R a=rwxs dir",
        ] {
            denied(&p, cmd);
        }
        for cmd in [
            "chmod 755 x",
            "chmod +x script.sh",
            "chmod -R u+rw dir",
            "chmod u-s x",
            "chmod 0755 x",
        ] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn git_validator() {
        let p = policy();
        for cmd in [
            "git config user.email a@b.c",
            "git config --global core.editor vim",
            "git -c user.name=x commit -m m",
            "git -c author.email=x commit",
            "git -c committer.name=x commit",
            "git -c core.sshCommand='sh -c id' fetch",
            "git push --force origin main",
            "git push -f origin master",
            "git push origin +main",
            "git push --force-with-lease origin HEAD:refs/heads/main",
            "git push origin --delete main",
            "git push origin :master",
            "git push --force",
            "git push -fu origin",
        ] {
            denied(&p, cmd);
        }
        for cmd in [
            "git status",
            "git commit -m 'fix: thing'",
            "git config --get user.name",
            "git config --list",
            "git -C sub log --oneline",
            "git -c color.ui=always diff",
            "git push origin feature/x",
            "git push --force origin vibe/task-1",
            "git push -u origin HEAD",
        ] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn kill_validator() {
        let p = policy();
        for cmd in [
            "kill -9 -1",
            "kill 0",
            "kill -- -1",
            "kill -TERM -0",
            "kill -1",
        ] {
            denied(&p, cmd);
        }
        for cmd in [
            "kill 1234",
            "kill -9 1234",
            "kill -s TERM 1234",
            "kill -l",
            "kill -0 1234",
        ] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn pkill_validator() {
        let p = policy();
        for cmd in [
            "pkill systemd",
            "killall -9 launchd",
            "pkill -f 'sshd: user'",
            "killall WindowServer",
            "killall explorer.exe",
            "pkill dockerd",
        ] {
            denied(&p, cmd);
        }
        for cmd in ["pkill -f 'node server.js'", "killall my-dev-server"] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn shells_are_validated_recursively() {
        let p = policy();
        denied(&p, "bash -c 'sudo ls'");
        denied(&p, "sh -c \"rm -rf /\"");
        denied(&p, "zsh -lc 'echo ok && reboot'");
        denied(&p, "bash -c \"bash -c 'sudo id'\"");
        denied(&p, "fish --command='sudo id'");
        denied(&p, "echo 'sudo id' | sh");
        denied(&p, "bash -s < script.sh");
        denied(&p, "bash -c");
        denied(&p, "bash -euo pipefail -c 'sudo id'");
        denied(&p, "bash -o errexit -c 'sudo id'");
        allowed(&p, "bash -c 'cargo test && echo ok'");
        allowed(&p, "sh ./scripts/build.sh --release");
        allowed(&p, "bash -euo pipefail -c 'make'");
    }

    #[test]
    fn eval_find_and_xargs_are_validated() {
        let p = policy();
        denied(&p, "eval 'sudo id'");
        denied(&p, "find . -name x -exec sudo rm {} \\;");
        denied(&p, "find / -maxdepth 0 -exec rm -rf / \\;");
        denied(&p, "ls | xargs sudo rm");
        allowed(&p, "find . -name '*.tmp' -exec rm {} +");
        allowed(&p, "find . -type f | xargs -n 1 wc -l");
    }

    #[test]
    fn database_clients() {
        let p = policy();
        for cmd in [
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
        ] {
            denied(&p, cmd);
        }
        for cmd in [
            "psql -c 'SELECT * FROM users'",
            "psql -c 'DELETE FROM users WHERE id = 3'",
            "redis-cli GET key",
            "mongosh --eval 'db.users.find()'",
            "psql -f migrations/001.sql",
        ] {
            allowed(&p, cmd);
        }
    }

    #[test]
    fn dropdb_only_for_disposable_names() {
        let p = policy();
        denied(&p, "dropdb production");
        denied(&p, "dropuser admin");
        denied(&p, "dropdb -h localhost -U postgres");
        allowed(&p, "dropdb app_test");
        allowed(&p, "dropdb -h localhost -U postgres myapp_dev");
        allowed(&p, "dropuser sandbox_user");
    }
}
