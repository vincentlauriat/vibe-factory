# Providers and models

A **provider** is a model back-end: Anthropic, any API speaking the OpenAI Chat Completions
dialect (OpenAI, Groq, Mistral, xAI, OpenRouter, Ollama, most gateways), or the built-in
mock. You declare providers in `.vibe/config.toml`, give each a name, and refer to models as
`<provider name>/<model id>`.

## Defaults

Without any configuration, Vibe Factory knows three providers and uses
`anthropic/claude-sonnet-5` for every phase with the `medium` thinking level:

```toml
default_provider = "anthropic"
default_model = "anthropic/claude-sonnet-5"

[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[providers.ollama]
kind = "openai"
base_url = "http://localhost:11434/v1"
default_model = "qwen2.5-coder"
```

These built-in values apply when `.vibe/config.toml` does not exist **and** when the file
contains no `[providers.*]` table at all. As soon as you declare one provider, the providers
are exactly the tables you wrote: declare every provider you use (a file written from the
defaults, as above, already contains the three).

A provider whose key is missing still loads. It fails with an authentication error on its
first call only, so listing providers you do not use is harmless.

## Provider keys

Every `[providers.<name>]` table accepts:

| Key | Type | Meaning |
|-----|------|---------|
| `kind` | string, required | `anthropic`, `openai`, `ollama` or `mock` (plugins may add kinds) |
| `api_key` | string | the key itself; avoid committing it, prefer `api_key_env` |
| `api_key_env` | string | environment variable holding the key (an empty value counts as missing) |
| `base_url` | string | endpoint override (gateway, proxy, self-hosted server); trailing `/` is ignored |
| `default_model` | string | model used when a reference names no model, or names `default` |
| `stream` | bool | stream answers as they are generated (default `true`); set `false` for a gateway that mishandles server-sent events |
| `extra` | table | kind-specific settings, below |

`api_key` wins over `api_key_env` when both are set.

An Anthropic API key that is not scoped to a workspace is refused with HTTP 400 ("must include
the anthropic-workspace-id header"). Set `ANTHROPIC_WORKSPACE_ID` to the workspace id
(`wrkspc_…`, from the console's workspace settings) and every `anthropic` provider sends it as
`anthropic-workspace-id`, or use a key created in a workspace. In the macOS app, add the
variable in Settings like the key.

### `extra` keys

| Key | Kinds | Type | Effect |
|-----|-------|------|--------|
| `headers` | anthropic, openai, ollama | table of strings | extra HTTP headers sent with every request |
| `thinking` | anthropic | `"adaptive"` or `"budget"` | force the thinking form instead of choosing it from the model id (see [Thinking levels](#thinking-levels)) |
| `auth` | anthropic | `"api_key"` (default) or `"bearer"` | `bearer` sends the key as `Authorization: Bearer` with the OAuth beta header instead of `x-api-key` |
| `require_api_key` | openai, ollama | bool | default `false` when `base_url` is set, `true` on the official OpenAI endpoint; with `false`, no `Authorization` header is sent when the key is missing |
| `max_tokens_param` | openai, ollama | string | name of the output-budget field; default `max_completion_tokens` on the official endpoint, `max_tokens` elsewhere |
| `reasoning` | openai, ollama | bool | send `reasoning_effort`; default on for the official endpoint only, since many compatible servers reject unknown fields |
| `reply` | mock | string | fixed answer of the mock provider (default `"Mock response."`) |

The `ollama` kind is the `openai` kind with `base_url` defaulting to
`http://localhost:11434/v1`.

```toml
[providers.gateway]
kind = "openai"
base_url = "https://llm.internal.example.com/v1"
api_key_env = "GATEWAY_TOKEN"

[providers.gateway.extra]
max_tokens_param = "max_tokens"

[providers.gateway.extra.headers]
"X-Team" = "platform"
```

## Referring to models

A model reference is `provider/model`, where `provider` is the *name* of a
`[providers.<name>]` table, not its kind. A reference without `/` is attributed to
`default_provider`.

| Reference | Resolves to |
|-----------|-------------|
| `anthropic/claude-opus-5` | provider `anthropic`, model `claude-opus-5` |
| `claude-opus-5` | provider `default_provider`, model `claude-opus-5` |
| `ollama/default` | provider `ollama`, its `default_model` |
| `openrouter/meta-llama/llama-4-maverick` | provider `openrouter`, model `meta-llama/llama-4-maverick` (split at the first `/`) |

On providers of kind `anthropic` these shorthands are expanded:

| Shorthand | Model id |
|-----------|----------|
| `opus` | `claude-opus-5` |
| `sonnet` | `claude-sonnet-5` |
| `haiku` | `claude-haiku-4-5-20251001` |
| `fable` | `claude-fable-5-1` |

An unknown provider name fails with ``unknown provider `x` for model `x/y` (configured: …)``,
and an unknown kind with ``provider `x` has unknown kind `y` (known kinds: anthropic, mock,
ollama, openai)``.

## Models per phase

Each pipeline phase can use its own model and, optionally, its own thinking level. Phases
without an entry use `default_model`, and every agent keeps its own thinking level.

```toml
default_model = "anthropic/sonnet"

[phases.assess]
model = "anthropic/haiku"
thinking = "low"

[phases.plan]
model = "anthropic/opus"
thinking = "high"

[phases.build]
model = "ollama/qwen2.5-coder"
thinking = "off"
```

The phase names are `assess`, `spec`, `plan`, `build`, `qa`, `fix` and `merge`. An agent can
also pin a model regardless of the phase; see [Customising agents](agents.md).

## Thinking levels

| Level | Budget | Anthropic, current models | Anthropic, older models | OpenAI-compatible |
|-------|--------|---------------------------|-------------------------|-------------------|
| `off` | none | no `thinking` | no `thinking` | no `reasoning_effort` |
| `low` | 1 024 tokens | adaptive, effort `low` | `budget_tokens: 1024` | `reasoning_effort = "low"` |
| `medium` | 4 096 | adaptive, effort `medium` | 4 096 | `"medium"` |
| `high` | 16 384 | adaptive, effort `high` | 16 384 | `"high"` |
| `max` | 32 768 | adaptive, effort `max` | 32 768 | `"high"` |

On OpenAI-compatible servers the level is only sent when `reasoning` is enabled (see
above); otherwise it is ignored.

With Anthropic, current models (Claude 4.6 and later: Sonnet 5, Opus 5 and 5.5, Fable, Opus
4.6 to 4.8, Sonnet 4.6) take `thinking: {type: "adaptive"}` with `output_config.effort` and
reject `budget_tokens` and sampling parameters, so vibe never sends a temperature to them.
Older models (Haiku 4.5, Claude 4.5 and before) and unknown model ids keep
`budget_tokens`, and thinking removes the temperature as the API requires. The form is
chosen from the model id; set `extra.thinking = "adaptive"` or `"budget"` on the provider to
force it, for example behind a gateway that renames models. Model ids are sent as is, so the
choice follows `claude-...` names.

## Recipes

**Ollama** (local, no key):

```toml
[providers.ollama]
kind = "ollama"
default_model = "qwen2.5-coder:32b"
```

**Groq**:

```toml
[providers.groq]
kind = "openai"
base_url = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"
extra = { require_api_key = true }
```

**OpenRouter**:

```toml
[providers.openrouter]
kind = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-sonnet-5"

[providers.openrouter.extra]
require_api_key = true

[providers.openrouter.extra.headers]
"X-Title" = "vibe-factory"
```

Reference it as `openrouter/anthropic/claude-sonnet-5`.

**xAI**:

```toml
[providers.xai]
kind = "openai"
base_url = "https://api.x.ai/v1"
api_key_env = "XAI_API_KEY"
extra = { require_api_key = true }
```

**Mistral**:

```toml
[providers.mistral]
kind = "openai"
base_url = "https://api.mistral.ai/v1"
api_key_env = "MISTRAL_API_KEY"
extra = { require_api_key = true }
```

Setting `require_api_key = true` on hosted services turns a forgotten environment variable
into a clear `no API key configured` error instead of an anonymous request rejected by the
server.

**Mock** (dry runs and tests, no network):

```toml
default_model = "fake/any"

[providers.fake]
kind = "mock"
extra = { reply = "Nothing to do." }
```

## Streaming

Anthropic and OpenAI-compatible providers stream their answers: the text and, when the
model exposes it, the reasoning arrive as `agent_delta` events while a step is being
generated (`vibe --json run` prints them; the terminal output shows the complete text at
the end of each step, as before). The final answer is identical to a non-streamed one.

A request that fails before any text arrived is retried as usual. A stream that breaks after
text was sent is not retried by the provider (the text would be repeated): the step fails
and the agent runtime's own retry policy applies. An OpenAI-compatible server that refuses
`stream` or `stream_options` with a 4xx error gets the same request without streaming.
`agent_delta` events are not written to `events.jsonl`.

## Retries and errors

HTTP providers retry transient failures on their own: four attempts in total, starting at
1 second and doubling up to 60 seconds, with ±25 % jitter. A `Retry-After` header from the
server replaces the computed delay, still capped at 60 seconds. On top of that, the agent
runtime retries a still-failing call twice more and publishes a `retrying` event you will
see in the live output.

| Situation | Classified as | Retried |
|-----------|---------------|---------|
| HTTP 429 or "rate limit" wording | rate limited | yes |
| HTTP 5xx, including 529 overloaded | server error | yes |
| DNS, TLS, connection or timeout failure, HTTP 408 | network | yes |
| HTTP 401 / 403 | authentication failed | no |
| billing, credit or quota wording, or HTTP 402 (even on a 429) | invalid request | no |
| HTTP 400 / 413 mentioning the context length | context too long | no; the agent session stops and may continue from a summary |
| other 4xx | invalid request | no |

Messages you may see:

```text
AuthFailed: no API key configured for provider `anthropic` (set `api_key` or `api_key_env`)
AuthFailed: HTTP 401: invalid x-api-key sk-***
InvalidRequest: HTTP 429: You exceeded your current quota
RateLimited: HTTP 429: rate limit reached for requests
ServerError: malformed provider response: no choices
Network: request timed out: …
```

Keys, bearer tokens and `token=` / `api_key=` values are masked before anything is logged or
shown. Requests time out after 30 seconds to connect and 300 seconds overall.

## Prompt caching

With Anthropic, the system prompt is sent as a cached block, so the many turns of one agent
session reuse it; cache reads and writes are reported in the token usage. OpenAI-compatible
servers that cache automatically report cached prompt tokens the same way. Nothing needs to
be configured.
