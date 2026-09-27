# Tools and security

Agents act on your project through **tools**. Every tool is confined to the task's
workspace and governed by a security policy you control from `.vibe/config.toml`.

## The built-in tools

| Tool | What it does | Needs |
|------|--------------|-------|
| `read_file` | Read a text file with line numbers (`offset`, `limit`, default 2000 lines). Binary files are refused. | read |
| `write_file` | Create or overwrite a file, creating parent directories. | write |
| `edit_file` | Replace an exact snippet (`old_string` → `new_string`, optional `replace_all`); returns a unified diff. | read + write |
| `list_dir` | Indented tree of a directory (`depth` 1–10, default 2), honouring `.gitignore`, skipping `.git`, `target`, `node_modules`. | read |
| `glob` | Find files by pattern (`**/*.rs`), newest first, at most 2000 results. | read |
| `grep` | Regex search in text files (`content`, `files` or `count` mode, context lines, case-insensitive). | read |
| `bash` | Run a shell command inside the workspace with a timeout (default 120 s, max 600 s). Background jobs are killed when the command finishes. | execute |
| `web_fetch` | Fetch a public http(s) page and return its text (HTML converted, 2 MB read at most, 30 000 characters returned). Hosts resolving to loopback, private, link-local or other internal addresses are refused, and each redirect is checked again; `security.web_allowed_domains` narrows it to some domains. | network |
| `web_search` | Search through the SearXNG-compatible endpoint of `security.search_url` and list the first 10 results. Only registered when that key is set. | network |

The web tools refuse to run unless `security.allow_network = true`. They are read-only
tools, so read-only agents such as the researcher and the reviewers get them too.

Which tools an agent may use is decided per role (see [Customising agents](agents.md)):
the planner and the reviewers of the spec phase are read-only, the coder and the QA agents
have everything.

## Path containment

Every path an agent gives is resolved against the workspace root and must stay inside it:

- `../secrets`, absolute paths and symbolic links that point outside are refused;
- a symbolic link whose target does not exist yet is refused as well, so a write can never
  create a file outside the workspace;
- writing tools additionally refuse to write *through* any symbolic link.

Paths listed in `security.extra_read_paths` may be read but never written. Nothing under a
`.git` directory can be written by the file tools, whatever the path used to reach it.

## The shell policy

The `bash` tool does not run a command before the **security policy** has approved it.
The command line is parsed into segments (pipes, `&&`, `||`, `;`, `&`, subshells,
`bash -c "…"`, `eval`, `find -exec`, `xargs`) and *every* segment is checked. Anything
the parser cannot understand is denied.

### Always blocked

System, disk, privilege and network administration:

```text
shutdown reboot halt poweroff init mkfs fdisk parted gdisk dd sudo su doas chown
iptables ip6tables nft ufw nmap systemctl service crontab mount umount
useradd userdel usermod groupadd groupdel passwd visudo
```

Program names computed at run time (`$CMD`, `$(…)`, globs, brace expansion) are denied,
and so are shells reading their program from standard input (`echo x | sh`).

### Guarded commands

| Command | Rule |
|---------|------|
| `rm`, `rmdir`, `unlink`, `shred` | No `--no-preserve-root`; no target containing `$`, a backtick, `*`, `?`, `[`, `{` or `~` (so `rm -rf ${HOME%/}`, `rm *.log` and `rm -rf /U*` are all refused); no `.`, `..`, the workspace root itself, or any absolute path outside the workspace (`/Users/you/.ssh` included). Symbolic links leading out of the workspace are followed for the check. |
| `find … -delete` / `-exec rm` | The start paths obey the `rm` rules. |
| `… \| xargs rm` | Every upstream command's path arguments obey the `rm` rules; `xargs` must name its program explicitly and cannot take it from its input. |
| Redirections `>`, `>>`, `&>`, `tee` | The target file must be inside the workspace and contain no expansion; `/dev/null` and friends are fine; `/dev/tcp` needs network access. |
| `cd`, `pushd` | The target must stay inside the workspace (no `cd ..` from the root, no `cd -`, no `CDPATH`). |
| `chmod` | No setuid/setgid bits (`4xxx`, `2xxx`, `6xxx`, `+s`). |
| `git` | `git config` only for reads (`get`, `list`, `--get*`, `-l`); no `-c` overrides of identity, hooks, pagers, editors, includes, filters, credential or GPG helpers; no `GIT_*` environment overrides; no `rebase -x`, `bisect run`, `filter-branch`, `submodule foreach`, `difftool`/`mergetool` commands; no force-push to `main`/`master` (or without an explicit branch); no deletion of `main`/`master`; no `--exec-path`. Everything else is allowed. |
| `kill`, `pkill`, `killall` | No `kill -1`/`kill 0`; no system processes (`launchd`, `systemd`, `sshd`, `dockerd`, …). |
| Shells (`bash`, `sh`, `zsh`, `fish`, `dash`, `ksh`, `csh`, `tcsh`, `rc`, `elvish`, `nu`, `pwsh`, `powershell`) | The inline command (`-c`, `-Command`) is validated recursively; reading the program from standard input and `-EncodedCommand` are refused. |
| Wrappers (`env`, `time`, `nice`, `nohup`, `xargs`, `timeout`, `setsid`, `stdbuf`, `caffeinate`, `flock`, `watch`, `strace`, `script`, `busybox`, …) | Stripped, then the wrapped command is validated; a wrapper that writes a file (`time -o`, `strace -o`, `script`) obeys the write rules. |
| `alias`, `hash`, `enable`, `expect -c` | Refused: they can give a blocked program another name or run code the policy cannot read. |
| Environment prefixes | `PATH`, `CDPATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`, `IFS`, `BASH_ENV`, `ENV`, `PROMPT_COMMAND`, `PERL5OPT`, `PYTHONSTARTUP`, `NODE_OPTIONS`, `RUBYOPT` and the `GIT_*` overrides cannot be set, in any form (`VAR=x cmd`, `env VAR=x`, `export`, `declare`, `read`, …). |
| `psql`, `mysql`, `mariadb`, `redis-cli`, `mongosh`, `mongo` | No `DROP`, `TRUNCATE`, `DELETE` without `WHERE`, `FLUSHALL`/`FLUSHDB`, `.drop(`, `dropDatabase`, `deleteMany({})`. |
| `dropdb`, `dropuser` | Only names containing `test`, `dev`, `local`, `tmp`, `temp`, `scratch`, `sandbox` or `mock`. |
| `curl`, `wget`, `nc`, `ssh`, `scp`, `sftp`, `ftp`, `telnet` | Only when network access is allowed (see below). |
| Anything touching `.git/` | Writing tools, copies, links and redirections into `.git/hooks`, `.git/config`, `.git/info`, `.git/worktrees` or `.git/modules` are refused, and `git worktree` is limited to `list`. |

### What you can configure

```toml
[security]
# Programs to block in addition to the built-in list.
blocked_commands = ["docker", "terraform"]

# If non-empty, ONLY these programs (and these wrappers, e.g. `nohup`) may run,
# plus harmless builtins: cd, echo, printf, true, false, pwd, test, [, exit, :.
allowed_commands = ["cargo", "git", "ls", "cat"]

# Default timeout of a shell command, in seconds (an agent may ask up to 600).
command_timeout_secs = 120

# Let agents use network clients (curl, wget, ssh, …).
allow_network = false

# Directories outside the project that agents may READ.
extra_read_paths = ["/usr/share/doc"]
```

When a command is denied, the agent receives the reason as a full sentence
("Command denied by the security policy: `rm` may not target `/`") and can choose another
approach.

## Output limits

Tool output shown to the model is capped (30 000 characters for `grep` and `bash`,
100 000 for any tool). The full text of a truncated output is saved under
`.vibe/tool-output/<uuid>.txt` and the path is given to the agent.

## Hooks: your own rules

The policy above is static. For dynamic rules ("no `npm publish` on Fridays", "log every
command to Slack", "ask me before touching migrations") write a **hook**, either in Rust or
as a plugin in any language; see [Plugins](plugins.md) and
[Extension points](../design/extension-points.md).

## What the policy is not

It is a **denylist and a set of validators, not a sandbox**. An agent that can run
`python -c` or `node -e` can, in principle, do anything the interpreter can. For untrusted
code or strict environments, combine the policy with an isolated workspace, keep
`allow_network = false`, and use the allowlist. The built-in
[container workspace](workspaces.md#container-workspace) runs every shell command in a
hardened, throw-away container with no network by default; a VM or remote sandbox can be
plugged in as a `WorkspaceProvider` plugin. The file tools still act on the host worktree,
inside the path containment described above.
