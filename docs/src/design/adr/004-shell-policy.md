# ADR-004: Denylist shell policy with validators

**Status:** Accepted — 2026-09-26

## Context

Agents need a shell to build, test and inspect a project. A pure allowlist breaks on every
new tool chain; a wide-open shell is dangerous. Real incidents come from a small set of
commands: destructive `rm`, privilege escalation, disk and system management, identity
tampering in git, and destructive database statements.

## Decision

Every command line is parsed into segments (pipes, `&&`, `;`, subshells, `bash -c`) and
every segment is checked against:

1. a built-in list of blocked programs (system, disk, privilege, network administration);
2. per-command validators for `rm`, `chmod`, `git`, `kill`, `pkill`, database clients and
   `dropdb`/`dropuser`;
3. the project configuration: extra blocked programs, an optional allowlist, and whether
   network clients (`curl`, `wget`) are permitted.

Unparsable input is denied. Every denial is a full sentence returned to the model so it
can choose another approach. Commands always run inside the workspace with a timeout.

## Consequences

- Works out of the box on any stack.
- Not a sandbox: a determined model could still find a harmful command. Containers or VM
  workspaces remain the answer for untrusted code; the policy is defence in depth.
- Teams with stricter needs switch on the allowlist.
