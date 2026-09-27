//! OpenAI-compatible Chat Completions provider.
//!
//! One implementation serves every backend speaking the Chat Completions
//! dialect (`POST {base_url}/chat/completions`): OpenAI, Groq, Mistral, xAI,
//! OpenRouter, Ollama (`http://localhost:11434/v1`) and most self-hosted
//! gateways. Differences are handled through configuration:
//!
//! * the API key is optional when [`OpenAiCompatibleProvider::allow_missing_key`]
//!   is set (local servers); no `Authorization` header is sent then;
//! * the output budget field defaults to `max_completion_tokens` on the
//!   official endpoint and `max_tokens` elsewhere (override with
//!   `extra.max_tokens_param`);
//! * a thinking budget becomes `reasoning_effort` (`low` / `medium` / `high`).
//!   This is on by default for the official endpoint only; other backends opt
//!   in with `extra.reasoning = true` (many reject unknown fields).
//!
//! Mapping: the system prompt is the first message; assistant tool calls go in
//! `tool_calls` with JSON-string `arguments`; each tool result becomes its own
//! `role: "tool"` message.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use vibe_core::provider::ProviderInfo;
use vibe_core::{
    CompletionRequest, CompletionResponse, ContentBlock, DeltaSink, Error, ErrorKind, Message,
    ModelProvider, ProviderConfig, Result, Role, StopReason, StreamDelta, Usage,
};

use crate::http::{
    headers_from_extra, malformed, missing_key_error, normalize_base_url, send_json, shared_client,
};
use crate::retry::{RetryPolicy, with_retry};
use crate::sse::{Flow, send_sse};

/// Official OpenAI endpoint.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Default model on the official endpoint.
pub const DEFAULT_MODEL: &str = "gpt-5";
/// Local Ollama endpoint.
pub const OLLAMA_BASE_URL: &str = "http://localhost:11434/v1";

/// Map a thinking budget onto a `reasoning_effort` value: up to 2k tokens is
/// `low`, up to 8k `medium`, anything above `high`. `None` or `0` disables it.
#[must_use]
pub fn reasoning_effort(budget: Option<u32>) -> Option<&'static str> {
    match budget? {
        0 => None,
        1..=2048 => Some("low"),
        2049..=8192 => Some("medium"),
        _ => Some("high"),
    }
}

/// [`ModelProvider`] speaking the Chat Completions protocol.
#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    allow_missing_key: bool,
    default_model: String,
    max_tokens_param: String,
    supports_thinking: bool,
    client: reqwest::Client,
    headers: HeaderMap,
    retry: RetryPolicy,
    streaming: bool,
}

impl fmt::Debug for OpenAiCompatibleProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleProvider")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("allow_missing_key", &self.allow_missing_key)
            .field("default_model", &self.default_model)
            .field("max_tokens_param", &self.max_tokens_param)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatibleProvider {
    /// Provider for the official OpenAI endpoint.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_api_key(Some(api_key.into()))
    }

    /// Provider for the official endpoint with an optional key. Without a key
    /// every call fails with [`vibe_core::ErrorKind::AuthFailed`] unless
    /// [`Self::allow_missing_key`] is set.
    #[must_use]
    pub fn with_api_key(api_key: Option<String>) -> Self {
        Self {
            name: "openai".into(),
            base_url: DEFAULT_BASE_URL.into(),
            api_key,
            allow_missing_key: false,
            default_model: DEFAULT_MODEL.into(),
            max_tokens_param: "max_completion_tokens".into(),
            supports_thinking: true,
            client: shared_client(),
            headers: HeaderMap::new(),
            retry: RetryPolicy::default(),
            streaming: true,
        }
    }

    /// Provider for a local Ollama server (no key required).
    #[must_use]
    pub fn ollama() -> Self {
        Self::with_api_key(None)
            .with_name("ollama")
            .with_base_url(OLLAMA_BASE_URL)
            .allow_missing_key(true)
            .with_default_model("qwen2.5-coder")
    }

    /// Build from a `[providers.<name>]` entry.
    ///
    /// A custom `base_url` makes the key optional (local or self-hosted
    /// servers); on the official endpoint a missing key yields
    /// [`vibe_core::ErrorKind::AuthFailed`] on first call. Recognised `extra`
    /// keys: `max_tokens_param` (string), `require_api_key` (bool),
    /// `reasoning` (bool, send `reasoning_effort`) and a `headers` table.
    pub fn from_config(name: &str, config: &ProviderConfig) -> Result<Self> {
        let mut provider = Self::with_api_key(config.resolve_api_key())
            .with_name(name)
            .with_headers(headers_from_extra(&config.extra)?);
        if let Some(url) = &config.base_url {
            provider = provider.with_base_url(url);
        }
        let custom_endpoint = provider.base_url != DEFAULT_BASE_URL;
        let allow_missing = match config.extra.get("require_api_key") {
            Some(v) => !v.as_bool().ok_or_else(|| {
                Error::config(format!(
                    "provider `{name}`: `extra.require_api_key` must be a boolean"
                ))
            })?,
            None => custom_endpoint,
        };
        provider = provider.allow_missing_key(allow_missing);
        if let Some(param) = config.extra.get("max_tokens_param") {
            let param = param.as_str().ok_or_else(|| {
                Error::config(format!(
                    "provider `{name}`: `extra.max_tokens_param` must be a string"
                ))
            })?;
            provider = provider.with_max_tokens_param(param);
        }
        if let Some(reasoning) = config.extra.get("reasoning") {
            let reasoning = reasoning.as_bool().ok_or_else(|| {
                Error::config(format!(
                    "provider `{name}`: `extra.reasoning` must be a boolean"
                ))
            })?;
            provider = provider.with_thinking_support(reasoning);
        }
        if let Some(model) = &config.default_model {
            provider = provider.with_default_model(model);
        }
        Ok(provider.with_streaming(config.stream.unwrap_or(true)))
    }

    /// Override the endpoint. Switching away from the official endpoint also
    /// switches the output budget field to `max_tokens` and disables
    /// `reasoning_effort` (re-enable with [`Self::with_thinking_support`]).
    #[must_use]
    pub fn with_base_url(mut self, url: impl AsRef<str>) -> Self {
        self.base_url = normalize_base_url(url.as_ref());
        let official = self.base_url == DEFAULT_BASE_URL;
        self.max_tokens_param = if official {
            "max_completion_tokens".into()
        } else {
            "max_tokens".into()
        };
        self.supports_thinking = official;
        self
    }

    /// Set the registered name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the default model.
    #[must_use]
    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Accept calls without an API key (no `Authorization` header is sent).
    #[must_use]
    pub fn allow_missing_key(mut self, allow: bool) -> Self {
        self.allow_missing_key = allow;
        self
    }

    /// Name of the JSON field carrying the output budget.
    #[must_use]
    pub fn with_max_tokens_param(mut self, param: impl Into<String>) -> Self {
        self.max_tokens_param = param.into();
        self
    }

    /// Whether to advertise and send reasoning effort.
    #[must_use]
    pub fn with_thinking_support(mut self, supported: bool) -> Self {
        self.supports_thinking = supported;
        self
    }

    /// Stream answers (default true). When false, `complete_streaming`
    /// makes a plain request and sends no delta.
    #[must_use]
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
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

    /// Endpoint URL of the Chat Completions API.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// Model id actually sent for `request`.
    #[must_use]
    pub fn effective_model(&self, request: &CompletionRequest) -> String {
        if request.model.trim().is_empty() {
            self.default_model.clone()
        } else {
            request.model.trim().to_string()
        }
    }

    /// Translate a request into the Chat Completions JSON body.
    #[must_use]
    pub fn build_body(&self, request: &CompletionRequest) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), json!(self.effective_model(request)));

        let mut messages = Vec::new();
        if !request.system.trim().is_empty() {
            messages.push(json!({"role": "system", "content": request.system}));
        }
        for message in &request.messages {
            messages.extend(message_to_wire(message));
        }
        body.insert("messages".into(), Value::Array(messages));
        body.insert(self.max_tokens_param.clone(), json!(request.max_tokens));

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }

        let effort = if self.supports_thinking {
            reasoning_effort(request.thinking_budget)
        } else {
            None
        };
        match effort {
            Some(effort) => {
                body.insert("reasoning_effort".into(), json!(effort));
            }
            None => {
                if let Some(t) = request.temperature {
                    body.insert("temperature".into(), json!(t));
                }
            }
        }
        if !request.stop_sequences.is_empty() {
            body.insert("stop".into(), json!(request.stop_sequences));
        }
        for (k, v) in &request.extra {
            body.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(body)
    }

    fn request_builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .post(self.endpoint())
            .headers(self.headers.clone());
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }
        rb.json(body)
    }
}

/// Convert one core message into zero or more wire messages.
fn message_to_wire(message: &Message) -> Vec<Value> {
    let mut out = Vec::new();
    let mut texts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } if !text.is_empty() => texts.push(text.as_str()),
            ContentBlock::Text { .. } | ContentBlock::Thinking { .. } => {}
            ContentBlock::ToolUse { id, name, input } => {
                let arguments = if input.is_null() {
                    "{}".to_string()
                } else {
                    input.to_string()
                };
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let content = if *is_error {
                    format!("Error: {content}")
                } else {
                    content.clone()
                };
                out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}));
            }
        }
    }
    let text = texts.join("\n");
    match message.role {
        Role::User => {
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text}));
            }
        }
        Role::Assistant => {
            if !text.is_empty() || !tool_calls.is_empty() {
                let mut m = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                });
                if !tool_calls.is_empty() {
                    m["tool_calls"] = Value::Array(tool_calls);
                }
                out.push(m);
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct WireResponse {
    choices: Vec<WireChoice>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    #[serde(default)]
    id: Option<String>,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<WirePromptDetails>,
}

#[derive(Deserialize, Default)]
struct WirePromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// A chat completion rebuilt from its stream of chunks, in the shape of a
/// non-streamed response so that [`parse_response`] reads both.
#[derive(Debug, Default)]
pub struct StreamedChat {
    model: Option<String>,
    content: String,
    reasoning: String,
    /// `(id, name, arguments)` by tool call index.
    tool_calls: Vec<(String, String, String)>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    done: bool,
    chunks: usize,
}

impl StreamedChat {
    /// Apply the `data` of one event, reporting new text through `on_delta`.
    pub fn apply(&mut self, data: &str, on_delta: &mut dyn FnMut(StreamDelta)) -> Result<Flow> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(Flow::Stop);
        }
        let chunk: Value = serde_json::from_str(data).map_err(malformed)?;
        if let Some(message) = chunk["error"]["message"].as_str() {
            return Err(Error::new(
                ErrorKind::ServerError,
                format!("stream error: {message}"),
            ));
        }
        self.chunks += 1;
        if let Some(model) = chunk["model"].as_str().filter(|m| !m.is_empty()) {
            self.model = Some(model.to_string());
        }
        if chunk["usage"].is_object() {
            self.usage = Some(chunk["usage"].clone());
        }
        let Some(choice) = chunk["choices"].as_array().and_then(|c| c.first()) else {
            return Ok(Flow::Continue);
        };
        let delta = &choice["delta"];
        for key in ["reasoning_content", "reasoning"] {
            if let Some(text) = delta[key].as_str().filter(|t| !t.is_empty()) {
                self.reasoning.push_str(text);
                on_delta(StreamDelta::Thinking { text: text.into() });
            }
        }
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            self.content.push_str(text);
            on_delta(StreamDelta::Text { text: text.into() });
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (position, call) in calls.iter().enumerate() {
                let index = call["index"]
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(position);
                if self.tool_calls.len() <= index {
                    self.tool_calls.resize(index + 1, Default::default());
                }
                let slot = &mut self.tool_calls[index];
                if let Some(id) = call["id"].as_str() {
                    slot.0.push_str(id);
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    slot.1.push_str(name);
                }
                if let Some(arguments) = call["function"]["arguments"].as_str() {
                    slot.2.push_str(arguments);
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_string());
        }
        Ok(Flow::Continue)
    }

    /// The complete response; an error if nothing usable arrived.
    pub fn finish(self) -> Result<Value> {
        if !self.done && self.finish_reason.is_none() {
            return Err(Error::new(
                ErrorKind::Network,
                "the stream ended before the completion finished",
            ));
        }
        if self.chunks == 0 {
            return Err(malformed("empty stream"));
        }
        let tool_calls: Vec<Value> = self
            .tool_calls
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, arguments)| {
                json!({"id": id, "type": "function",
                       "function": {"name": name, "arguments": arguments}})
            })
            .collect();
        let mut message = json!({"role": "assistant", "content": self.content});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        let mut value = json!({
            "choices": [{"message": message, "finish_reason": self.finish_reason}],
        });
        if let Some(model) = self.model {
            value["model"] = json!(model);
        }
        if let Some(usage) = self.usage {
            value["usage"] = usage;
        }
        Ok(value)
    }
}

/// Map a `finish_reason` onto [`StopReason`].
///
/// `length` always yields [`StopReason::MaxTokens`], even with tool calls
/// present (their arguments are likely truncated). Otherwise a response
/// carrying tool calls is [`StopReason::ToolUse`], whatever the backend
/// reported.
#[must_use]
pub fn map_finish_reason(reason: Option<&str>, has_tool_calls: bool) -> StopReason {
    if reason == Some("length") {
        return StopReason::MaxTokens;
    }
    if has_tool_calls {
        return StopReason::ToolUse;
    }
    match reason {
        Some("stop") | None => StopReason::EndTurn,
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

/// Key under which undecodable tool call arguments are preserved (see the
/// crate docs, "Invalid tool arguments").
pub const RAW_ARGUMENTS_KEY: &str = "_raw";

/// Decode tool call arguments: a JSON string holding an object is parsed, an
/// object is taken as is, absent or blank arguments become `{}`. Anything
/// else (invalid or truncated JSON, a non-object value) is preserved verbatim
/// as `{"_raw": "<text>"}` instead of being silently replaced.
fn parse_arguments(arguments: Option<Value>) -> Value {
    match arguments {
        None | Some(Value::Null) => json!({}),
        Some(Value::String(s)) if s.trim().is_empty() => json!({}),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v @ Value::Object(_)) => v,
            _ => json!({ RAW_ARGUMENTS_KEY: s }),
        },
        Some(v @ Value::Object(_)) => v,
        Some(other) => json!({ RAW_ARGUMENTS_KEY: other.to_string() }),
    }
}

/// Parse a Chat Completions JSON response. Shape errors are reported as
/// [`vibe_core::ErrorKind::ServerError`].
pub fn parse_response(value: Value, requested_model: &str) -> Result<CompletionResponse> {
    let wire: WireResponse = serde_json::from_value(value).map_err(malformed)?;
    let choice = wire
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| malformed("no choices"))?;

    let mut content = Vec::new();
    if let Some(reasoning) = choice
        .message
        .reasoning_content
        .or(choice.message.reasoning)
        .filter(|r| !r.is_empty())
    {
        content.push(ContentBlock::Thinking { text: reasoning });
    }
    if let Some(text) = choice.message.content.filter(|t| !t.is_empty()) {
        content.push(ContentBlock::Text { text });
    }
    let has_tool_calls = !choice.message.tool_calls.is_empty();
    for (i, call) in choice.message.tool_calls.into_iter().enumerate() {
        content.push(ContentBlock::ToolUse {
            id: call
                .id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("call_{i}")),
            name: call.function.name,
            input: parse_arguments(call.function.arguments),
        });
    }

    let usage = wire.usage.unwrap_or_default();
    Ok(CompletionResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason: map_finish_reason(choice.finish_reason.as_deref(), has_tool_calls),
        usage: Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_tokens: usage.prompt_tokens_details.map_or(0, |d| d.cached_tokens),
            cache_write_tokens: 0,
        },
        model: wire
            .model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| requested_model.to_string()),
    })
}

#[async_trait::async_trait]
impl ModelProvider for OpenAiCompatibleProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            supports_tools: true,
            supports_thinking: self.supports_thinking,
            default_model: self.default_model.clone(),
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        if self.api_key.is_none() && !self.allow_missing_key {
            return Err(missing_key_error(&self.name));
        }
        let model = self.effective_model(&request);
        let body = self.build_body(&request);
        let value = with_retry(&self.retry, || send_json(self.request_builder(&body))).await?;
        parse_response(value, &model)
    }

    async fn complete_streaming(
        &self,
        request: CompletionRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<CompletionResponse> {
        if !self.streaming {
            return self.complete(request).await;
        }
        if self.api_key.is_none() && !self.allow_missing_key {
            return Err(missing_key_error(&self.name));
        }
        let model = self.effective_model(&request);
        let mut body = self.build_body(&request);
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage": true});
        let emitted = AtomicBool::new(false);
        let streamed = with_retry(&self.retry, || async {
            let mut stream = StreamedChat::default();
            let result = send_sse(self.request_builder(&body), |event| {
                stream.apply(&event.data, &mut |delta| {
                    emitted.store(true, Ordering::Relaxed);
                    on_delta(delta);
                })
            })
            .await
            .and_then(|()| stream.finish());
            match result {
                Ok(value) => Ok(value),
                Err(e) if emitted.load(Ordering::Relaxed) => Err(Error::new(
                    ErrorKind::Other,
                    format!("the stream broke after output started: {}", e.message),
                )),
                Err(e) => Err(e),
            }
        })
        .await;
        match streamed {
            Ok(value) => parse_response(value, &model),
            // Some compatible servers refuse `stream` or `stream_options`:
            // answer without streaming rather than failing.
            Err(e) if e.kind == ErrorKind::InvalidRequest && !emitted.load(Ordering::Relaxed) => {
                tracing::debug!(error = %e.message, "streaming refused, completing without it");
                self.complete(request).await
            }
            Err(e) => Err(e),
        }
    }

    async fn health_check(&self) -> Result<()> {
        if self.api_key.is_none() && !self.allow_missing_key {
            return Err(missing_key_error(&self.name));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use vibe_core::{ErrorKind, ToolSpec};

    #[test]
    fn efforts() {
        assert_eq!(reasoning_effort(None), None);
        assert_eq!(reasoning_effort(Some(0)), None);
        assert_eq!(reasoning_effort(Some(1024)), Some("low"));
        assert_eq!(reasoning_effort(Some(4096)), Some("medium"));
        assert_eq!(reasoning_effort(Some(16384)), Some("high"));
    }

    #[test]
    fn body_maps_messages_and_tools() {
        let mut req = CompletionRequest::new(
            "gpt-5",
            vec![
                Message::user("go"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking { text: "t".into() },
                        ContentBlock::ToolUse {
                            id: "c1".into(),
                            name: "read".into(),
                            input: json!({"path": "a"}),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::Text {
                            text: "also".into(),
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "c1".into(),
                            content: "nope".into(),
                            is_error: true,
                        },
                    ],
                },
            ],
        );
        req.system = "sys".into();
        req.temperature = Some(0.5);
        req.stop_sequences = vec!["X".into()];
        req.tools = vec![ToolSpec {
            name: "read".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }];
        let body = OpenAiCompatibleProvider::new("k").build_body(&req);
        assert_eq!(
            body,
            json!({
                "model": "gpt-5",
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "user", "content": "go"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}
                    ]},
                    {"role": "tool", "tool_call_id": "c1", "content": "Error: nope"},
                    {"role": "user", "content": "also"}
                ],
                "max_completion_tokens": 8192,
                "tools": [{"type": "function", "function": {"name": "read", "description": "d", "parameters": {"type": "object"}}}],
                "temperature": 0.5,
                "stop": ["X"]
            })
        );
    }

    #[test]
    fn reasoning_effort_replaces_temperature() {
        let mut req = CompletionRequest::new("", vec![Message::user("x")]);
        req.thinking_budget = Some(4096);
        req.temperature = Some(0.1);
        let body = OpenAiCompatibleProvider::ollama().build_body(&req);
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["temperature"], json!(0.1f32));
        let p = OpenAiCompatibleProvider::ollama().with_thinking_support(true);
        let body = p.build_body(&req);
        assert_eq!(body["model"], "qwen2.5-coder");
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["max_tokens"], 8192);
        assert!(body.get("temperature").is_none());
        let body = p.with_thinking_support(false).build_body(&req);
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("temperature").is_some());
    }

    #[test]
    fn parses_tool_calls_and_usage() {
        let value = json!({
            "model": "gpt-5-2026",
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "thinking...",
                    "tool_calls": [
                        {"id": "call_a", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"x\"}"}},
                        {"type": "function", "function": {"name": "ls", "arguments": ""}},
                        {"id": "call_c", "type": "function", "function": {"name": "bad", "arguments": "{oops"}},
                        {"id": "call_d", "type": "function", "function": {"name": "obj", "arguments": {"k": 1}}}
                    ]
                }
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 8}}
        });
        let r = parse_response(value, "req").unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.model, "gpt-5-2026");
        assert_eq!(
            r.message.content[0],
            ContentBlock::Thinking {
                text: "thinking...".into()
            }
        );
        let calls: Vec<_> = r
            .message
            .tool_uses()
            .map(|(i, n, v)| (i.to_string(), n.to_string(), v.clone()))
            .collect();
        assert_eq!(
            calls[0],
            ("call_a".into(), "read".into(), json!({"path": "x"}))
        );
        assert_eq!(calls[1], ("call_1".into(), "ls".into(), json!({})));
        assert_eq!(calls[2].2, json!({"_raw": "{oops"}));
        assert_eq!(calls[3].2, json!({"k": 1}));
        assert_eq!(r.usage.input_tokens, 12);
        assert_eq!(r.usage.cache_read_tokens, 8);
    }

    #[test]
    fn finish_reasons() {
        assert_eq!(map_finish_reason(Some("stop"), false), StopReason::EndTurn);
        assert_eq!(map_finish_reason(None, false), StopReason::EndTurn);
        assert_eq!(
            map_finish_reason(Some("tool_calls"), false),
            StopReason::ToolUse
        );
        assert_eq!(
            map_finish_reason(Some("length"), false),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_finish_reason(Some("length"), true),
            StopReason::MaxTokens
        );
        assert_eq!(
            map_finish_reason(Some("content_filter"), false),
            StopReason::Other
        );
    }

    #[test]
    fn truncated_tool_arguments_are_preserved_and_flag_max_tokens() {
        let truncated = r#"{"path":"a.rs","content":"fn ma"#;
        let value = json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "write_file", "arguments": truncated}},
                    {"id": "c2", "type": "function", "function": {"name": "n", "arguments": "[1,2]"}}
                ]}
            }]
        });
        let r = parse_response(value, "m").unwrap();
        assert_eq!(r.stop_reason, StopReason::MaxTokens);
        let inputs: Vec<_> = r.message.tool_uses().map(|(_, _, v)| v.clone()).collect();
        assert_eq!(inputs[0], json!({"_raw": truncated}));
        assert_eq!(inputs[1], json!({"_raw": "[1,2]"}));
        assert_eq!(parse_arguments(None), json!({}));
        assert_eq!(parse_arguments(Some(json!(""))), json!({}));
    }

    #[test]
    fn empty_choices_is_malformed() {
        let err = parse_response(json!({"choices": []}), "m").unwrap_err();
        assert_eq!(err.kind, ErrorKind::ServerError);
    }

    #[test]
    fn config_parsing() {
        let cfg = ProviderConfig {
            kind: "openai".into(),
            base_url: Some("http://localhost:11434/v1/".into()),
            ..Default::default()
        };
        let p = OpenAiCompatibleProvider::from_config("local", &cfg).unwrap();
        assert!(p.allow_missing_key);
        assert!(!p.supports_thinking);
        assert_eq!(p.max_tokens_param, "max_tokens");
        assert_eq!(p.endpoint(), "http://localhost:11434/v1/chat/completions");

        let cfg = ProviderConfig {
            kind: "openai".into(),
            api_key: Some("sk-secret-123456".into()),
            ..Default::default()
        };
        let p = OpenAiCompatibleProvider::from_config("openai", &cfg).unwrap();
        assert!(!p.allow_missing_key);
        assert!(p.supports_thinking);
        assert!(!format!("{p:?}").contains("secret"));

        let mut cfg: ProviderConfig = toml::from_str(
            "kind = \"openai\"\nbase_url = \"https://api.groq.com/openai/v1\"\n[extra]\nrequire_api_key = true\nmax_tokens_param = \"max_completion_tokens\"\n",
        )
        .unwrap();
        let p = OpenAiCompatibleProvider::from_config("groq", &cfg).unwrap();
        assert!(!p.allow_missing_key);
        assert!(!p.supports_thinking);
        assert_eq!(p.max_tokens_param, "max_completion_tokens");
        cfg.extra
            .insert("reasoning".into(), toml::Value::Boolean(true));
        assert!(
            OpenAiCompatibleProvider::from_config("groq", &cfg)
                .unwrap()
                .supports_thinking
        );
        cfg.extra
            .insert("require_api_key".into(), toml::Value::String("yes".into()));
        assert_eq!(
            OpenAiCompatibleProvider::from_config("groq", &cfg)
                .unwrap_err()
                .kind,
            ErrorKind::Config
        );
    }

    #[tokio::test]
    async fn missing_key_on_official_endpoint_fails_fast() {
        let p = OpenAiCompatibleProvider::with_api_key(None);
        let req = CompletionRequest::new("m", vec![Message::user("x")]);
        assert_eq!(
            p.complete(req).await.unwrap_err().kind,
            ErrorKind::AuthFailed
        );
        assert!(
            OpenAiCompatibleProvider::ollama()
                .health_check()
                .await
                .is_ok()
        );
    }
}
