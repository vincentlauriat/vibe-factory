//! Anthropic Messages API provider.
//!
//! Maps [`CompletionRequest`] onto `POST {base_url}/v1/messages` and the
//! answer back onto [`CompletionResponse`]:
//!
//! * text, tool use and tool result blocks map one to one; thinking blocks
//!   returned by the model become [`ContentBlock::Thinking`];
//! * the API requires the signed thinking blocks of a tool-use turn to be sent
//!   back verbatim, and the core message model carries no signature. The
//!   provider therefore keeps the raw thinking blocks of recent responses in a
//!   bounded in-process cache keyed by the turn's first tool call id and
//!   replays them. When they are unavailable (e.g. a resumed session), extended
//!   thinking is disabled for that request so it stays valid;
//! * the system prompt is sent as a single cached block
//!   (`cache_control: {type: "ephemeral"}`) so repeated agent turns reuse it;
//! * `thinking_budget` enables extended thinking and raises `max_tokens` above
//!   the budget when needed (temperature is omitted, as the API requires);
//! * cache read/creation token counts are reported in [`Usage`].
//!
//! Authentication uses `x-api-key` by default, or `Authorization: Bearer` plus
//! the OAuth beta header when [`AnthropicAuth::Bearer`] is selected (config:
//! `extra.auth = "bearer"`).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, ContentBlock, Error, Message, ModelProvider,
    ProviderConfig, Result, Role, StopReason, Usage,
};

use crate::http::{
    headers_from_extra, malformed, missing_key_error, normalize_base_url, send_json, shared_client,
};
use crate::retry::{RetryPolicy, with_retry};

/// Public API endpoint.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Model used when neither the request nor the configuration names one.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
/// Value of the `anthropic-version` header.
pub const API_VERSION: &str = "2023-06-01";
/// Beta flag required when authenticating with an OAuth bearer token.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Smallest thinking budget the API accepts.
pub const MIN_THINKING_BUDGET: u32 = 1024;
/// Number of tool-use turns whose signed thinking blocks are remembered.
pub const THINKING_CACHE_CAPACITY: usize = 512;

/// Raw (signed) thinking blocks of recent responses, keyed by the id of the
/// first tool call of the same assistant turn.
#[derive(Default)]
struct ThinkingCache {
    order: VecDeque<String>,
    blocks: HashMap<String, Vec<Value>>,
}

impl ThinkingCache {
    fn insert(&mut self, key: String, blocks: Vec<Value>) {
        if self.blocks.insert(key.clone(), blocks).is_none() {
            self.order.push_back(key);
            while self.order.len() > THINKING_CACHE_CAPACITY {
                if let Some(old) = self.order.pop_front() {
                    self.blocks.remove(&old);
                }
            }
        }
    }

    fn get(&self, key: &str) -> Option<&Vec<Value>> {
        self.blocks.get(key)
    }
}

/// Lock a mutex, recovering the data if another thread panicked.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Expand a model shorthand (`opus`, `sonnet`, `haiku`, `fable`) to a full
/// model id. Anything else is returned unchanged.
#[must_use]
pub fn expand_model_shorthand(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
        "opus" => "claude-opus-5".into(),
        "sonnet" => "claude-sonnet-5".into(),
        "haiku" => "claude-haiku-4-5-20251001".into(),
        "fable" => "claude-fable-5-1".into(),
        _ => model.trim().to_string(),
    }
}

/// Credentials for the Messages API.
#[derive(Clone, PartialEq, Eq)]
pub enum AnthropicAuth {
    /// Classic API key sent as `x-api-key`.
    ApiKey(String),
    /// OAuth access token sent as `Authorization: Bearer`.
    Bearer(String),
}

impl fmt::Debug for AnthropicAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey(***)"),
            Self::Bearer(_) => f.write_str("Bearer(***)"),
        }
    }
}

/// [`ModelProvider`] backed by the Anthropic Messages API.
#[derive(Clone)]
pub struct AnthropicProvider {
    name: String,
    base_url: String,
    auth: Option<AnthropicAuth>,
    default_model: String,
    client: reqwest::Client,
    headers: HeaderMap,
    retry: RetryPolicy,
    thinking_cache: Arc<Mutex<ThinkingCache>>,
}

impl fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("auth", &self.auth)
            .field("default_model", &self.default_model)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Provider using an API key against the public endpoint.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_auth(Some(AnthropicAuth::ApiKey(api_key.into())))
    }

    /// Provider with explicit (possibly absent) credentials. Without
    /// credentials every call fails with [`vibe_core::ErrorKind::AuthFailed`].
    #[must_use]
    pub fn with_auth(auth: Option<AnthropicAuth>) -> Self {
        Self {
            name: "anthropic".into(),
            base_url: DEFAULT_BASE_URL.into(),
            auth,
            default_model: DEFAULT_MODEL.into(),
            client: shared_client(),
            headers: HeaderMap::new(),
            retry: RetryPolicy::default(),
            thinking_cache: Arc::new(Mutex::new(ThinkingCache::default())),
        }
    }

    /// Build from a `[providers.<name>]` entry.
    ///
    /// Recognised `extra` keys: `auth = "bearer" | "api_key"` and a
    /// `headers` table. A missing key is not an error here.
    pub fn from_config(name: &str, config: &ProviderConfig) -> Result<Self> {
        let key = config.resolve_api_key();
        let auth_mode = config
            .extra
            .get("auth")
            .map(|v| {
                v.as_str().map(str::to_ascii_lowercase).ok_or_else(|| {
                    Error::config(format!("provider `{name}`: `extra.auth` must be a string"))
                })
            })
            .transpose()?;
        let auth = match auth_mode.as_deref() {
            None | Some("api_key" | "x-api-key" | "key") => key.map(AnthropicAuth::ApiKey),
            Some("bearer" | "oauth") => key.map(AnthropicAuth::Bearer),
            Some(other) => {
                return Err(Error::config(format!(
                    "provider `{name}`: unknown auth mode `{other}` (expected `api_key` or `bearer`)"
                )));
            }
        };
        let mut provider = Self::with_auth(auth)
            .with_name(name)
            .with_headers(headers_from_extra(&config.extra)?);
        if let Some(url) = &config.base_url {
            provider = provider.with_base_url(url);
        }
        if let Some(model) = &config.default_model {
            provider = provider.with_default_model(model);
        }
        Ok(provider)
    }

    /// Switch to OAuth bearer authentication with `token`.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.auth = Some(AnthropicAuth::Bearer(token.into()));
        self
    }

    /// Override the endpoint (gateways, proxies, tests).
    #[must_use]
    pub fn with_base_url(mut self, url: impl AsRef<str>) -> Self {
        self.base_url = normalize_base_url(url.as_ref());
        self
    }

    /// Set the registered name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the default model (shorthands are expanded).
    #[must_use]
    pub fn with_default_model(mut self, model: impl AsRef<str>) -> Self {
        self.default_model = expand_model_shorthand(model.as_ref());
        self
    }

    /// Set the retry policy.
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Add headers sent with every request.
    #[must_use]
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Use a specific HTTP client instead of the shared one.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Endpoint URL of the Messages API.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    /// Model id actually sent for `request` (default model when empty,
    /// shorthands expanded).
    #[must_use]
    pub fn effective_model(&self, request: &CompletionRequest) -> String {
        if request.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            expand_model_shorthand(&request.model)
        }
    }

    /// Translate a request into the Messages API JSON body.
    #[must_use]
    pub fn build_body(&self, request: &CompletionRequest) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), json!(self.effective_model(request)));

        let messages: Vec<Value> = {
            let cache = lock(&self.thinking_cache);
            request
                .messages
                .iter()
                .filter_map(|m| message_to_wire(m, &cache))
                .collect()
        };
        let budget = request
            .thinking_budget
            .filter(|b| *b > 0)
            .map(|b| b.max(MIN_THINKING_BUDGET))
            .filter(|_| thinking_compatible(&messages));
        let max_tokens = match budget {
            Some(b) if request.max_tokens <= b => b.saturating_add(request.max_tokens.max(1)),
            _ => request.max_tokens,
        };
        body.insert("max_tokens".into(), json!(max_tokens));

        if !request.system.trim().is_empty() {
            body.insert(
                "system".into(),
                json!([{
                    "type": "text",
                    "text": request.system,
                    "cache_control": {"type": "ephemeral"},
                }]),
            );
        }

        body.insert("messages".into(), Value::Array(messages));

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }

        match budget {
            Some(b) => {
                body.insert(
                    "thinking".into(),
                    json!({"type": "enabled", "budget_tokens": b}),
                );
            }
            None => {
                if let Some(t) = request.temperature {
                    body.insert("temperature".into(), json!(t));
                }
            }
        }

        if !request.stop_sequences.is_empty() {
            body.insert("stop_sequences".into(), json!(request.stop_sequences));
        }

        // Opaque options never override the fields computed above.
        for (k, v) in &request.extra {
            body.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(body)
    }

    fn request_builder(&self, auth: &AnthropicAuth, body: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .post(self.endpoint())
            .header("anthropic-version", API_VERSION)
            .headers(self.headers.clone());
        rb = match auth {
            AnthropicAuth::ApiKey(key) => rb.header("x-api-key", key),
            AnthropicAuth::Bearer(token) => {
                rb.bearer_auth(token).header("anthropic-beta", OAUTH_BETA)
            }
        };
        rb.json(body)
    }
}

/// Convert one core message into the wire format, or `None` when nothing
/// sendable remains (e.g. a message made only of thinking blocks).
fn message_to_wire(message: &Message, cache: &ThinkingCache) -> Option<Value> {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    // Signed thinking blocks recorded for this turn go first, verbatim.
    let mut content: Vec<Value> = match (message.role, message.tool_uses().next()) {
        (Role::Assistant, Some((id, _, _))) => cache.get(id).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    content.extend(message.content.iter().filter_map(|block| match block {
        ContentBlock::Text { text } if text.is_empty() => None,
        ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
        ContentBlock::Thinking { .. } => None,
        ContentBlock::ToolUse { id, name, input } => {
            let input = if input.is_null() {
                json!({})
            } else {
                input.clone()
            };
            Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut block = json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
            });
            if *is_error {
                block["is_error"] = json!(true);
            }
            Some(block)
        }
    }));
    (!content.is_empty()).then(|| json!({"role": role, "content": content}))
}

/// Whether extended thinking may be enabled for these wire messages: the API
/// requires the last assistant turn, when it calls tools, to start with its
/// own thinking block.
fn thinking_compatible(messages: &[Value]) -> bool {
    let Some(last_assistant) = messages.iter().rev().find(|m| m["role"] == "assistant") else {
        return true;
    };
    let blocks = last_assistant["content"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let has_tool_use = blocks.iter().any(|b| b["type"] == "tool_use");
    let starts_with_thinking = blocks
        .first()
        .is_some_and(|b| b["type"] == "thinking" || b["type"] == "redacted_thinking");
    !has_tool_use || starts_with_thinking
}

/// Raw thinking blocks of a response and the id of its first tool call.
fn signed_thinking(value: &Value) -> Option<(String, Vec<Value>)> {
    let blocks = value.get("content")?.as_array()?;
    let tool_id = blocks
        .iter()
        .find(|b| b["type"] == "tool_use")?
        .get("id")?
        .as_str()?
        .to_string();
    let thinking: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "thinking" || b["type"] == "redacted_thinking")
        .cloned()
        .collect();
    (!thinking.is_empty()).then_some((tool_id, thinking))
}

#[derive(Deserialize)]
struct WireResponse {
    content: Vec<WireBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    RedactedThinking {},
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

/// Map the API `stop_reason` string onto [`StopReason`].
#[must_use]
pub fn map_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        _ => StopReason::Other,
    }
}

/// Parse a Messages API JSON response. Shape errors are reported as
/// [`vibe_core::ErrorKind::ServerError`].
pub fn parse_response(value: Value, requested_model: &str) -> Result<CompletionResponse> {
    let wire: WireResponse = serde_json::from_value(value).map_err(malformed)?;
    let content = wire
        .content
        .into_iter()
        .filter_map(|block| match block {
            WireBlock::Text { text } => Some(ContentBlock::Text { text }),
            WireBlock::Thinking { thinking } => Some(ContentBlock::Thinking { text: thinking }),
            WireBlock::ToolUse { id, name, input } => Some(ContentBlock::ToolUse {
                id,
                name,
                input: if input.is_null() { json!({}) } else { input },
            }),
            WireBlock::RedactedThinking {} | WireBlock::Unknown => None,
        })
        .collect();
    Ok(CompletionResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason: map_stop_reason(wire.stop_reason.as_deref()),
        usage: Usage {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
            cache_read_tokens: wire.usage.cache_read_input_tokens.unwrap_or(0),
            cache_write_tokens: wire.usage.cache_creation_input_tokens.unwrap_or(0),
        },
        model: wire.model.unwrap_or_else(|| requested_model.to_string()),
    })
}

#[async_trait::async_trait]
impl ModelProvider for AnthropicProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            supports_tools: true,
            supports_thinking: true,
            default_model: self.default_model.clone(),
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| missing_key_error(&self.name))?;
        let model = self.effective_model(&request);
        let body = self.build_body(&request);
        let value =
            with_retry(&self.retry, || send_json(self.request_builder(auth, &body))).await?;
        if let Some((tool_id, blocks)) = signed_thinking(&value) {
            lock(&self.thinking_cache).insert(tool_id, blocks);
        }
        parse_response(value, &model)
    }

    async fn health_check(&self) -> Result<()> {
        match self.auth {
            Some(_) => Ok(()),
            None => Err(missing_key_error(&self.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use vibe_core::ToolSpec;

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new("sk-ant-test")
    }

    #[test]
    fn shorthands() {
        assert_eq!(expand_model_shorthand("opus"), "claude-opus-5");
        assert_eq!(expand_model_shorthand("Sonnet"), "claude-sonnet-5");
        assert_eq!(expand_model_shorthand("haiku"), "claude-haiku-4-5-20251001");
        assert_eq!(expand_model_shorthand("fable"), "claude-fable-5-1");
        assert_eq!(expand_model_shorthand("claude-x"), "claude-x");
    }

    #[test]
    fn body_maps_everything() {
        let mut req = CompletionRequest::new(
            "sonnet",
            vec![
                Message::user("read it"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking { text: "hmm".into() },
                        ContentBlock::Text { text: "ok".into() },
                        ContentBlock::ToolUse {
                            id: "t1".into(),
                            name: "read".into(),
                            input: json!({"path": "a"}),
                        },
                    ],
                },
                Message::tool_results(vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "boom".into(),
                    is_error: true,
                }]),
            ],
        );
        req.system = "be nice".into();
        req.temperature = Some(0.2);
        req.stop_sequences = vec!["END".into()];
        req.tools = vec![ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];
        req.extra.insert("top_p".into(), json!(0.9));
        req.extra.insert("model".into(), json!("ignored"));

        let body = provider().build_body(&req);
        assert_eq!(
            body,
            json!({
                "model": "claude-sonnet-5",
                "max_tokens": 8192,
                "system": [{"type": "text", "text": "be nice", "cache_control": {"type": "ephemeral"}}],
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "read it"}]},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": "ok"},
                        {"type": "tool_use", "id": "t1", "name": "read", "input": {"path": "a"}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "t1", "content": "boom", "is_error": true}
                    ]}
                ],
                "tools": [{"name": "read", "description": "Read a file", "input_schema": {"type": "object"}}],
                "temperature": 0.2f32,
                "stop_sequences": ["END"],
                "top_p": 0.9
            })
        );
    }

    #[test]
    fn thinking_raises_max_tokens_and_drops_temperature() {
        let mut req = CompletionRequest::new("", vec![Message::user("x")]);
        req.thinking_budget = Some(16384);
        req.temperature = Some(1.0);
        let body = provider().build_body(&req);
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 16384})
        );
        assert_eq!(body["max_tokens"], 16384 + 8192);
        assert!(body.get("temperature").is_none());
        assert!(body.get("system").is_none());

        req.thinking_budget = Some(100);
        req.max_tokens = 4000;
        let body = provider().build_body(&req);
        assert_eq!(body["thinking"]["budget_tokens"], MIN_THINKING_BUDGET);
        assert_eq!(body["max_tokens"], 4000);

        req.thinking_budget = Some(0);
        assert!(provider().build_body(&req).get("thinking").is_none());
    }

    fn tool_loop(thinking: bool) -> CompletionRequest {
        let mut assistant = vec![ContentBlock::ToolUse {
            id: "toolu_9".into(),
            name: "read".into(),
            input: json!({}),
        }];
        if thinking {
            assistant.insert(
                0,
                ContentBlock::Thinking {
                    text: "plan".into(),
                },
            );
        }
        let mut req = CompletionRequest::new(
            "m",
            vec![
                Message::user("go"),
                Message {
                    role: Role::Assistant,
                    content: assistant,
                },
                Message::tool_results(vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_9".into(),
                    content: "ok".into(),
                    is_error: false,
                }]),
            ],
        );
        req.thinking_budget = Some(4096);
        req.temperature = Some(0.3);
        req
    }

    #[test]
    fn tool_turn_without_signed_thinking_disables_thinking() {
        let body = provider().build_body(&tool_loop(true));
        assert!(body.get("thinking").is_none());
        assert_eq!(body["max_tokens"], 8192);
        assert!(body.get("temperature").is_some());
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn cached_signed_thinking_is_replayed_first() {
        let p = provider();
        let response = json!({
            "content": [
                {"type": "thinking", "thinking": "plan", "signature": "sig-1"},
                {"type": "redacted_thinking", "data": "opaque"},
                {"type": "tool_use", "id": "toolu_9", "name": "read", "input": {}}
            ]
        });
        let (id, blocks) = signed_thinking(&response).unwrap();
        lock(&p.thinking_cache).insert(id, blocks);

        let body = p.build_body(&tool_loop(true));
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 4096})
        );
        assert!(body.get("temperature").is_none());
        assert_eq!(
            body["messages"][1]["content"],
            json!([
                {"type": "thinking", "thinking": "plan", "signature": "sig-1"},
                {"type": "redacted_thinking", "data": "opaque"},
                {"type": "tool_use", "id": "toolu_9", "name": "read", "input": {}}
            ])
        );
        // Clones share the cache.
        assert!(
            p.clone()
                .build_body(&tool_loop(false))
                .get("thinking")
                .is_some()
        );
    }

    #[test]
    fn thinking_cache_is_bounded() {
        let mut cache = ThinkingCache::default();
        for i in 0..THINKING_CACHE_CAPACITY + 10 {
            cache.insert(format!("id{i}"), vec![json!({})]);
        }
        assert_eq!(cache.blocks.len(), THINKING_CACHE_CAPACITY);
        assert!(cache.get("id0").is_none());
        assert!(
            cache
                .get(&format!("id{}", THINKING_CACHE_CAPACITY + 9))
                .is_some()
        );
        assert!(signed_thinking(&json!({"content": [{"type": "text", "text": "x"}]})).is_none());
    }

    #[test]
    fn thinking_only_message_is_skipped() {
        let req = CompletionRequest::new(
            "m",
            vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking { text: "x".into() }],
            }],
        );
        assert_eq!(provider().build_body(&req)["messages"], json!([]));
    }

    #[test]
    fn parses_response_blocks_usage_and_stop() {
        let value = json!({
            "id": "msg_1",
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "let me see", "signature": "sig"},
                {"type": "redacted_thinking", "data": "xx"},
                {"type": "text", "text": "Reading."},
                {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {"path": "a"}},
                {"type": "server_tool_use", "id": "s", "name": "web", "input": {}}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 7, "cache_creation_input_tokens": 3}
        });
        let r = parse_response(value, "req-model").unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.model, "claude-sonnet-5");
        assert_eq!(
            r.message.content,
            vec![
                ContentBlock::Thinking {
                    text: "let me see".into()
                },
                ContentBlock::Text {
                    text: "Reading.".into()
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read".into(),
                    input: json!({"path": "a"}),
                },
            ]
        );
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 7,
                cache_write_tokens: 3
            }
        );
    }

    #[test]
    fn stop_reasons() {
        assert_eq!(map_stop_reason(Some("end_turn")), StopReason::EndTurn);
        assert_eq!(map_stop_reason(Some("max_tokens")), StopReason::MaxTokens);
        assert_eq!(
            map_stop_reason(Some("stop_sequence")),
            StopReason::StopSequence
        );
        assert_eq!(map_stop_reason(Some("pause_turn")), StopReason::Other);
        assert_eq!(map_stop_reason(None), StopReason::Other);
    }

    #[test]
    fn malformed_shape_is_server_error() {
        let err = parse_response(json!({"nope": 1}), "m").unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::ServerError);
    }

    #[test]
    fn config_parsing() {
        let cfg: ProviderConfig = toml::from_str(
            r#"
kind = "anthropic"
api_key = "tok"
base_url = "http://localhost:1/"
default_model = "opus"
[extra]
auth = "bearer"
[extra.headers]
x-team = "core"
"#,
        )
        .unwrap();
        let p = AnthropicProvider::from_config("claude", &cfg).unwrap();
        assert_eq!(p.auth, Some(AnthropicAuth::Bearer("tok".into())));
        assert_eq!(p.endpoint(), "http://localhost:1/v1/messages");
        assert_eq!(p.info().default_model, "claude-opus-5");
        assert_eq!(p.info().name, "claude");
        assert_eq!(p.headers.get("x-team").unwrap(), "core");
        assert!(!format!("{p:?}").contains("tok\""));

        let mut bad = cfg.clone();
        bad.extra
            .insert("auth".into(), toml::Value::String("magic".into()));
        assert!(AnthropicProvider::from_config("c", &bad).is_err());
    }

    #[tokio::test]
    async fn missing_key_fails_fast() {
        let p = AnthropicProvider::with_auth(None).with_base_url("http://127.0.0.1:9");
        let err = p
            .complete(CompletionRequest::new("m", vec![Message::user("x")]))
            .await
            .unwrap_err();
        assert_eq!(err.kind, vibe_core::ErrorKind::AuthFailed);
        assert_eq!(
            p.health_check().await.unwrap_err().kind,
            vibe_core::ErrorKind::AuthFailed
        );
        assert!(provider().health_check().await.is_ok());
    }
}
