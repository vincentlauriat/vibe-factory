//! # vibe-core
//!
//! Core domain model, traits and extension points of the **Vibe Factory**
//! framework: an open, extensible engine that turns a task description into
//! working software through a pipeline of cooperating AI agents.
//!
//! This crate is deliberately dependency-light and contains **no I/O and no
//! network code**. Everything concrete (LLM providers, tools, git isolation,
//! the agent loop, the pipeline, plugins, the CLI) lives in sibling crates and
//! plugs into the traits defined here:
//!
//! | Extension point | Trait | Purpose |
//! |-----------------|-------|---------|
//! | Model backends  | [`ModelProvider`] | Talk to any LLM API |
//! | Tools           | [`Tool`] | Give agents capabilities (files, shell, web, …) |
//! | Agents          | [`AgentSpec`] | Declarative agent definitions (prompt, tools, model) |
//! | Workspaces      | [`WorkspaceProvider`] | Isolate a task's work (git worktree, plain dir, container, …) |
//! | Memory          | [`MemoryStore`] | Persist and recall knowledge across runs |
//! | Persistence     | [`TaskStore`] | Store tasks, specs, plans and reports |
//! | Hooks           | [`Hook`] | Observe or veto phases and tool calls |
//! | Plugins         | [`Plugin`] | Bundle any of the above and register them in a [`Registry`] |
//!
//! The pipeline itself is described by [`Phase`] and driven by events
//! ([`Event`]) published on an [`EventBus`].

#![forbid(unsafe_code)]

pub mod agent;
pub mod config;
pub mod error;
pub mod event;
pub mod ids;
pub mod memory;
pub mod message;
pub mod phase;
pub mod plan;
pub mod plugin;
pub mod prompt;
pub mod provider;
pub mod qa;
pub mod spec;
pub mod store;
pub mod task;
pub mod tool;
pub mod workspace;

pub use agent::{
    AgentOutcome, AgentRole, AgentSpec, AgentStop, ModelSelection, ThinkingLevel, ToolSelection,
};
pub use config::{
    PhaseModels, PipelineConfig, PluginConfig, ProviderConfig, SecurityConfig, VibeConfig,
};
pub use error::{Error, ErrorKind, Result};
pub use event::{Envelope, Event, EventBus, EventSink};
pub use ids::{RunId, SessionId, SubtaskId, TaskId};
pub use memory::{InMemoryStore, MemoryEntry, MemoryKind, MemoryStore, SharedMemory};
pub use message::{ContentBlock, Message, Role};
pub use phase::Phase;
pub use plan::{Plan, PlanPhase, Subtask, SubtaskStatus};
pub use plugin::{Hook, HookDecision, Plugin, Registry, SharedHook};
pub use prompt::PromptTemplate;
pub use provider::{
    CompletionRequest, CompletionResponse, ModelProvider, ModelRef, ProviderInfo, SharedProvider,
    StopReason, ToolSpec, Usage,
};
pub use qa::{QaIssue, QaReport, QaVerdict, Severity};
pub use spec::{Requirement, RequirementKind, Spec, SpecContext};
pub use store::{SharedTaskStore, TaskStore};
pub use task::{Complexity, Task, TaskSource, TaskStatus};
pub use tool::{Permissions, SharedTool, Tool, ToolContext, ToolOutput, ToolRegistry};
pub use workspace::{
    InPlaceWorkspace, MergeOutcome, SharedWorkspaceProvider, Workspace, WorkspaceKind,
    WorkspaceProvider,
};
