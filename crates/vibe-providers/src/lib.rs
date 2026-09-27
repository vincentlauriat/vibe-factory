//! # vibe-providers
//!
//! LLM provider implementations for the **Vibe Factory** framework.
//!
//! Every backend implements [`vibe_core::ModelProvider`]: it receives a
//! provider-neutral [`vibe_core::CompletionRequest`] (system prompt, messages
//! made of text / thinking / tool use / tool result blocks, tool specs, output
//! and thinking budgets) and returns a [`vibe_core::CompletionResponse`]. The
//! rest of the framework never sees a vendor wire format.
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`anthropic`] | [`AnthropicProvider`] over the Messages API (tool use, extended thinking, prompt caching, API key or OAuth bearer) |
//! | [`openai`] | [`OpenAiCompatibleProvider`] over Chat Completions (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama, gateways) |
//! | [`mock`] | [`MockProvider`], a scripted provider for tests and dry runs |
//! | [`registry`] | [`ProviderRegistry`], named instances built from `[providers.*]` configuration |
//! | [`retry`] | [`RetryPolicy`] and [`with_retry`], exponential backoff for transient failures |
//! | [`classify`] | HTTP error classification into [`vibe_core::ErrorKind`] and secret scrubbing |
//! | [`http`] | The shared HTTP client, timeouts and custom headers |
//!
//! ## Error semantics
//!
//! Providers retry rate limits, 5xx responses and network failures themselves
//! (see [`RetryPolicy`]); callers only see an error once retries are exhausted,
//! with `retry_after` preserved. Authentication, billing and malformed-request
//! errors are never retried. A missing API key is reported as
//! [`vibe_core::ErrorKind::AuthFailed`] on the first call, not at construction,
//! so a configuration listing unused providers still loads.
//!
//! ## Invalid tool arguments
//!
//! When a backend returns tool call arguments that are not a JSON object
//! (typically truncated JSON because the output budget ran out), the call is
//! kept as a [`vibe_core::ContentBlock::ToolUse`] whose `input` is
//! `{"_raw": "<original text>"}` ([`openai::RAW_ARGUMENTS_KEY`]). Tool runtimes
//! should treat an input containing `_raw` as undecodable and report it to the
//! model instead of executing the tool. When the backend reported truncation
//! (`finish_reason = "length"`), the response's stop reason is
//! [`vibe_core::StopReason::MaxTokens`] even though tool calls are present, so
//! the caller's max-tokens handling applies.
//!
//! ## Adding a provider
//!
//! 1. Implement [`vibe_core::ModelProvider`] for your type. Reuse
//!    [`http::send_json`] and [`with_retry`] to get consistent error
//!    classification and retries for free.
//! 2. Teach the registry its `kind` so it can be declared in configuration:
//!
//! ```
//! use std::sync::Arc;
//! use vibe_core::provider::{ProviderInfo, SharedProvider};
//! use vibe_core::{CompletionRequest, CompletionResponse, ModelProvider, Result, VibeConfig};
//! use vibe_providers::{ProviderRegistry, mock::text_response};
//!
//! struct Echo;
//!
//! #[async_trait::async_trait]
//! impl ModelProvider for Echo {
//!     fn info(&self) -> ProviderInfo {
//!         ProviderInfo {
//!             name: "echo".into(),
//!             supports_tools: false,
//!             supports_thinking: false,
//!             default_model: "echo-1".into(),
//!         }
//!     }
//!
//!     async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
//!         let last = request.messages.last().map(|m| m.text()).unwrap_or_default();
//!         Ok(text_response(last))
//!     }
//! }
//!
//! let config = VibeConfig::from_toml("[providers.echo]\nkind = \"echo\"\n").unwrap();
//! let registry = ProviderRegistry::builder()
//!     .register_kind("echo", |_name, _cfg| Ok(Arc::new(Echo) as SharedProvider))
//!     .build(&config)
//!     .unwrap();
//! assert!(registry.get("echo").is_some());
//! ```
//!
//! Ready-made instances can also be added with
//! [`ProviderRegistry::register`].

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod classify;
pub mod http;
pub mod mock;
pub mod openai;
pub mod registry;
pub mod retry;
pub mod sse;

pub use anthropic::{AnthropicAuth, AnthropicProvider, expand_model_shorthand};
pub use classify::{classify_http_error, scrub_secrets};
pub use mock::{MockProvider, MockStep};
pub use openai::OpenAiCompatibleProvider;
pub use registry::{ProviderFactory, ProviderRegistry, ProviderRegistryBuilder};
pub use retry::{RetryPolicy, with_retry};
