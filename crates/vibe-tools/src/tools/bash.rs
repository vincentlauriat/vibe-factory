//! `bash`: run a shell command inside the workspace.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use vibe_core::config::SecurityConfig;
use vibe_core::{Error, Result, Tool, ToolContext, ToolOutput};

use super::common::{
    MAX_OUTPUT_CHARS, display_path, parse_input, permission_denied, resolve_inside_workspace,
    try_output, workspace_root,
};
use crate::security::output::truncate_output;
use crate::security::policy::SecurityPolicy;

/// Hard upper bound on a command timeout, in seconds.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// Bytes of raw output kept from the beginning and from the end of a stream.
const CAPTURE_EDGE_BYTES: usize = 64 * 1024;

/// How long to wait for output pipes to close after the shell exits (a
/// background child may keep them open).
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Runs shell commands after checking them against a [`SecurityPolicy`].
#[derive(Debug, Clone)]
pub struct BashTool {
    policy: SecurityPolicy,
    default_timeout_secs: u64,
}

impl BashTool {
    /// Build the tool from the project's security configuration.
    #[must_use]
    pub fn new(config: &SecurityConfig) -> Self {
        Self {
            policy: SecurityPolicy::new(config),
            default_timeout_secs: config.command_timeout_secs.clamp(1, MAX_TIMEOUT_SECS),
        }
    }

    /// The policy commands are checked against.
    #[must_use]
    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Merged stdout/stderr capture keeping the head and the tail.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: usize,
}

impl Capture {
    fn push(&mut self, mut chunk: &[u8]) {
        if self.head.len() < CAPTURE_EDGE_BYTES {
            let take = chunk.len().min(CAPTURE_EDGE_BYTES - self.head.len());
            self.head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
        self.tail.extend(chunk);
        let excess = self.tail.len().saturating_sub(CAPTURE_EDGE_BYTES);
        if excess > 0 {
            self.tail.drain(..excess);
            self.dropped += excess;
        }
    }

    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if self.dropped > 0 {
            text.push_str(&format!("\n\n[... {} bytes omitted ...]\n\n", self.dropped));
        }
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        text.push_str(&String::from_utf8_lossy(&tail));
        text
    }
}

async fn pump(mut reader: impl AsyncRead + Unpin, capture: Arc<Mutex<Capture>>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut c) = capture.lock() {
                    c.push(&buf[..n]);
                }
            }
        }
    }
}

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

#[cfg(unix)]
fn isolate_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_cmd: &mut Command) {}

/// Kill the whole process tree rooted at `pid`.
#[cfg(unix)]
async fn kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Kill the whole process tree rooted at `pid`.
#[cfg(not(unix))]
async fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace (through `sh -c`, or `cmd /C` on Windows) and \
         return its combined stdout and stderr followed by the exit code. Use it for builds, \
         tests, git and other command-line tools; prefer `read_file`, `grep`, `glob` and \
         `edit_file` for reading, searching and editing files. Every command is checked by a \
         security policy: privilege escalation, system administration, destructive `rm` \
         targets, force-pushes to main/master, git identity changes and (unless network \
         access is granted) network tools are refused with an explanation. `cwd` is a \
         directory inside the workspace (default: the root). The command is killed after \
         `timeout_secs` (default from the project configuration, at most 600). Output longer \
         than 30000 characters keeps its beginning and end. Commands run non-interactively \
         with no standard input."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command line to run."},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Timeout in seconds (default from configuration)."},
                "cwd": {"type": "string", "description": "Working directory relative to the workspace root (default: the root)."}
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        if !ctx.permissions.execute {
            return Ok(permission_denied("execute commands"));
        }
        let input: Input = try_output!(parse_input(self.name(), input));
        if let Err(reason) = self
            .policy
            .validate_with_network(&input.command, ctx.permissions.network)
        {
            let mut out =
                ToolOutput::error(format!("Command denied by the security policy: {reason}"));
            out.metadata = json!({"denied": true});
            return Ok(out);
        }
        let cwd = try_output!(resolve_inside_workspace(
            ctx,
            input.cwd.as_deref().unwrap_or(".")
        ));
        if !cwd.is_dir() {
            return Ok(ToolOutput::error(format!(
                "Working directory `{}` is not an existing directory.",
                display_path(&workspace_root(ctx), &cwd)
            )));
        }
        let timeout_secs = input
            .timeout_secs
            .unwrap_or(self.default_timeout_secs)
            .clamp(1, MAX_TIMEOUT_SECS);

        let mut cmd = shell_command(&input.command);
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .kill_on_drop(true);
        isolate_process_group(&mut cmd);

        let started = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput::error(format!("Cannot start the shell: {e}.")));
            }
        };
        let capture = Arc::new(Mutex::new(Capture::default()));
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(tokio::spawn(pump(out, Arc::clone(&capture))));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(tokio::spawn(pump(err, Arc::clone(&capture))));
        }

        let status = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;
        let (status, timed_out) = match status {
            Ok(Ok(status)) => (Some(status), false),
            Ok(Err(e)) => {
                return Err(Error::tool(format!("waiting for the shell failed: {e}")));
            }
            Err(_) => {
                if let Some(pid) = child.id() {
                    kill_tree(pid).await;
                }
                let _ = child.kill().await;
                (None, true)
            }
        };

        let mut pipes_left_open = false;
        for reader in readers {
            let abort = reader.abort_handle();
            if tokio::time::timeout(PIPE_DRAIN_GRACE, reader)
                .await
                .is_err()
            {
                abort.abort();
                pipes_left_open = true;
            }
        }
        let elapsed = started.elapsed();
        let raw = capture.lock().map(|c| c.text()).unwrap_or_default();
        let mut content = truncate_output(raw.trim_end(), MAX_OUTPUT_CHARS);
        if content.is_empty() {
            content.push_str("(no output)");
        }
        if pipes_left_open {
            content.push_str(
                "\n\n(A background process still holds the output streams; its further output \
                 is not captured.)",
            );
        }
        let exit_code = status.and_then(|s| s.code());
        let footer = if timed_out {
            format!("Command timed out after {timeout_secs} seconds and was killed.")
        } else {
            match exit_code {
                Some(code) => format!("Exit code: {code}"),
                None => "Exit code: none (terminated by a signal)".to_string(),
            }
        };
        content.push_str("\n\n");
        content.push_str(&footer);
        let success = !timed_out && exit_code == Some(0);
        let mut out = if success {
            ToolOutput::ok(content)
        } else {
            ToolOutput::error(content)
        };
        out.metadata = json!({
            "exit_code": exit_code,
            "timed_out": timed_out,
            "duration_ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::common::test_support::{workspace, write};
    use vibe_core::Permissions;

    fn tool() -> BashTool {
        BashTool::new(&SecurityConfig::default())
    }

    async fn run(ctx: &ToolContext, input: serde_json::Value) -> ToolOutput {
        tool().call(ctx, input).await.unwrap()
    }

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut c = Capture::default();
        c.push(&vec![b'a'; CAPTURE_EDGE_BYTES]);
        c.push(&[b'b'; 10]);
        c.push(&vec![b'z'; CAPTURE_EDGE_BYTES]);
        let text = c.text();
        assert!(text.starts_with('a'));
        assert!(text.ends_with('z'));
        assert!(text.contains("[... 10 bytes omitted ...]"));
    }

    #[tokio::test]
    async fn runs_commands_and_reports_exit_code() {
        let (_dir, ctx) = workspace();
        let out = run(&ctx, json!({"command": "echo hello"})).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "hello\n\nExit code: 0");
        let fail = run(&ctx, json!({"command": "exit 3"})).await;
        assert!(fail.is_error);
        assert!(fail.content.ends_with("Exit code: 3"));
        assert_eq!(fail.metadata["exit_code"], 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn merges_stderr_and_uses_cwd() {
        let (dir, ctx) = workspace();
        write(&dir, "sub/marker.txt", "x");
        let out = run(&ctx, json!({"command": "ls; echo oops >&2", "cwd": "sub"})).await;
        assert!(out.content.contains("marker.txt"));
        assert!(out.content.contains("oops"));
    }

    #[tokio::test]
    async fn policy_denials_are_reported_to_the_model() {
        let (_dir, ctx) = workspace();
        for cmd in [
            "sudo ls",
            "rm -rf /",
            "curl https://example.com",
            "git push -f origin main",
        ] {
            let out = run(&ctx, json!({"command": cmd})).await;
            assert!(out.is_error, "{cmd}");
            assert!(
                out.content
                    .starts_with("Command denied by the security policy"),
                "{cmd}"
            );
            assert_eq!(out.metadata["denied"], true);
        }
    }

    #[tokio::test]
    async fn network_permission_unlocks_network_tools() {
        let (_dir, ctx) = workspace();
        let ctx = ctx.with_permissions(Permissions::all());
        // `curl --version` does not touch the network; it only has to pass the policy.
        let out = run(&ctx, json!({"command": "curl --version"})).await;
        assert!(!out.content.contains("security policy"), "{}", out.content);
    }

    #[tokio::test]
    async fn execute_permission_is_required() {
        let (_dir, ctx) = workspace();
        let ctx = ctx.with_permissions(Permissions::read_only());
        let out = run(&ctx, json!({"command": "echo hi"})).await;
        assert!(out.is_error && out.content.contains("Permission denied"));
    }

    #[tokio::test]
    async fn cwd_must_stay_inside_the_workspace() {
        let (dir, ctx) = workspace();
        write(&dir, "f.txt", "");
        for cwd in ["..", "/", "f.txt", "missing"] {
            let out = run(&ctx, json!({"command": "echo hi", "cwd": cwd})).await;
            assert!(out.is_error, "{cwd}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let (dir, ctx) = workspace();
        let started = Instant::now();
        let out = run(
            &ctx,
            json!({"command": "sleep 30 & sleep 30; touch finished", "timeout_secs": 1}),
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("timed out after 1 seconds"));
        assert_eq!(out.metadata["timed_out"], true);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!dir.path().join("finished").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_children_do_not_hang_the_tool() {
        let (_dir, ctx) = workspace();
        let started = Instant::now();
        let out = run(&ctx, json!({"command": "sleep 20 & echo started"})).await;
        assert!(out.content.contains("started"));
        assert!(out.content.contains("Exit code: 0"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn long_output_is_truncated() {
        let (_dir, ctx) = workspace();
        let out = run(
            &ctx,
            json!({"command": "i=0; while [ $i -lt 5000 ]; do echo line-$i-xxxxxxxxxxxxxxxx; i=$((i+1)); done"}),
        )
        .await;
        assert!(out.content.contains("characters omitted"));
        assert!(out.content.contains("line-0-"));
        assert!(out.content.contains("line-4999-"));
        assert!(out.content.len() < MAX_OUTPUT_CHARS + 200);
    }

    #[test]
    fn timeout_default_is_clamped() {
        let cfg = SecurityConfig {
            command_timeout_secs: 10_000,
            ..SecurityConfig::default()
        };
        assert_eq!(BashTool::new(&cfg).default_timeout_secs, MAX_TIMEOUT_SECS);
        assert!(tool().policy().validate("ls").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cwd_symlink_pointing_outside_is_denied() {
        let (dir, ctx) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let out = run(&ctx, json!({"command": "touch pwned", "cwd": "escape"})).await;
        assert!(
            out.is_error && out.content.contains("Access denied"),
            "{}",
            out.content
        );
        assert!(!outside.path().join("pwned").exists());
    }
}
