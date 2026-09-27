# ADR-005: JSON-RPC over stdio plugin protocol

**Status:** Accepted — 2026-09-26

## Context

Rust traits are the native extension mechanism, but requiring Rust for every extension
contradicts the goal of openness. Meanwhile the Model Context Protocol (MCP) has made
"JSON-RPC 2.0 over stdio, `tools/list`, `tools/call`" a de facto standard with hundreds of
existing servers.

## Decision

Out-of-process plugins speak the **Vibe Plugin Protocol**: newline-delimited JSON-RPC 2.0
over stdin/stdout. The method names and payload shapes for tools follow MCP so that an MCP
stdio server works unchanged as a tool plugin. Additional methods (`agents/list`,
`hooks/before_tool`) expose the other extension points.

Plugins are declared in `.vibe/config.toml` or discovered from `vibe-plugin.toml`
manifests in `.vibe/plugins/` and the user configuration directory. A `PluginServer`
helper makes writing a plugin in Rust a few lines; any language can implement the wire
contract directly.

## Consequences

- Immediate access to the MCP ecosystem for tools.
- Plugins crash in their own process; a non-required plugin failure never stops a run.
- Latency per call (process boundary, JSON); acceptable next to model latency.
- Native in-process plugins remain available for performance-sensitive extensions.
