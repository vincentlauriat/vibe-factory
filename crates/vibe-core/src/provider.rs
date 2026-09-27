//! Model providers: the boundary between the framework and any LLM API.

use std::fmt;
use std::sync::Arc;

use crate::error::Result;
use crate::message::Message;

/// Reference to a model on a provider, e.g. `anthropic/claude-sonnet-5`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ModelRef {
    /// Provider name as registered in the [`crate::Registry`].
    pub provider: String,
    /// Provider-specific model identifier.
    pub model: String,
}

impl ModelRef {
    /// Build a reference.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// Parse `provider/model`. A bare `model` is attributed to
    /// `default_provider`; `provider/` (empty model) selects that provider's
    /// default model (empty `model` field).
    pub fn parse(s: &str, default_provider: &str) -> Self {
        match s.split_once('/') {
            Some((p, m)) if !p.is_empty() => Self::new(p, m),
            _ => Self::new(default_provider, s),
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// Description of a tool as advertised to the model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolSpec {
    /// Tool name (must be unique within a request).
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// JSON schema of the input object.
    pub input_schema: serde_json::Value,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of the answer.
    EndTurn,
    /// The model wants tools to be executed.
    ToolUse,
    /// The output token budget was exhausted.
    MaxTokens,
    /// A stop sequence was hit.
    StopSequence,
    /// Anything else the provider reported.
    Other,
}

/// Token accounting for one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Completion tokens.
    pub output_tokens: u64,
    /// Prompt tokens served from cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Prompt tokens written to cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl Usage {
    /// Sum two usages.
    #[must_use]
    pub fn combined(self, other: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cache_read_tokens: self.cache_read_tokens + other.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + other.cache_write_tokens,
        }
    }

    /// Total tokens.
    #[must_use]
    pub fn total(self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.combined(rhs);
    }
}

/// A completion request.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompletionRequest {
    /// Provider-specific model id.
    pub model: String,
    /// System prompt.
    #[serde(default)]
    pub system: String,
    /// Conversation so far.
    pub messages: Vec<Message>,
    /// Tools the model may call.
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// Output token budget.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Sampling temperature, if the provider supports it.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Reasoning budget in tokens, if the provider supports extended thinking.
    #[serde(default)]
    pub thinking_budget: Option<u32>,
    /// Stop sequences.
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    /// Opaque provider-specific options (`{"top_p": 0.9}`, …).
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_max_tokens() -> u32 {
    8192
}

impl CompletionRequest {
    /// Minimal request.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            max_tokens: default_max_tokens(),
            temperature: None,
            thinking_budget: None,
            stop_sequences: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }

    /// Rough token estimate of the whole prompt.
    #[must_use]
    pub fn estimate_tokens(&self) -> usize {
        self.system.len().div_ceil(4)
            + self
                .messages
                .iter()
                .map(Message::estimate_tokens)
                .sum::<usize>()
    }
}

/// A completion response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompletionResponse {
    /// The assistant message.
    pub message: Message,
    /// Why generation stopped.
    pub stop_reason: StopReason,
    /// Token usage.
    #[serde(default)]
    pub usage: Usage,
    /// Model that actually answered (may differ from the request on fallback).
    #[serde(default)]
    pub model: String,
}

/// A piece of a completion, delivered while it is being generated.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StreamDelta {
    /// Visible answer text.
    Text {
        /// The new text.
        text: String,
    },
    /// Reasoning text, when the provider exposes it.
    Thinking {
        /// The new text.
        text: String,
    },
}

/// Receives the [`StreamDelta`]s of a streamed completion, in order.
pub type DeltaSink<'a> = &'a (dyn Fn(StreamDelta) + Send + Sync);

/// Static information about a provider.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderInfo {
    /// Registered name.
    pub name: String,
    /// Whether the provider supports tool calling.
    pub supports_tools: bool,
    /// Whether the provider exposes extended thinking.
    pub supports_thinking: bool,
    /// Default model when none is configured.
    pub default_model: String,
}

/// A backend able to produce completions.
///
/// Implementations must be cheap to clone behind an [`Arc`] and safe to call
/// concurrently. Retries and backoff are the provider's responsibility so that
/// callers can stay simple; return [`crate::ErrorKind::RateLimited`] with a
/// `retry_after` hint only once retries are exhausted.
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    /// Static capabilities.
    fn info(&self) -> ProviderInfo;

    /// Produce one completion.
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;

    /// Produce one completion, calling `on_delta` with text as it is
    /// generated. The returned response is complete, exactly as
    /// [`ModelProvider::complete`] would return it; deltas are only a
    /// preview. The default calls `complete` and sends no delta.
    ///
    /// Implementations must not retry once a delta was sent (the caller
    /// would see the text twice): an interrupted stream is an error.
    async fn complete_streaming(
        &self,
        request: CompletionRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<CompletionResponse> {
        let _ = on_delta;
        self.complete(request).await
    }

    /// Verify credentials and connectivity. Default: succeed.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}

/// Shared handle to a provider.
pub type SharedProvider = Arc<dyn ModelProvider>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ref_parse() {
        assert_eq!(
            ModelRef::parse("openai/gpt-5", "anthropic"),
            ModelRef::new("openai", "gpt-5")
        );
        assert_eq!(
            ModelRef::parse("claude-sonnet-5", "anthropic"),
            ModelRef::new("anthropic", "claude-sonnet-5")
        );
        assert_eq!(ModelRef::new("a", "b").to_string(), "a/b");
        assert_eq!(
            ModelRef::parse("ollama/", "anthropic"),
            ModelRef::new("ollama", "")
        );
    }

    #[test]
    fn usage_sum() {
        let mut u = Usage {
            input_tokens: 1,
            output_tokens: 2,
            ..Default::default()
        };
        u += Usage {
            input_tokens: 3,
            output_tokens: 4,
            ..Default::default()
        };
        assert_eq!(u.total(), 10);
    }
}
