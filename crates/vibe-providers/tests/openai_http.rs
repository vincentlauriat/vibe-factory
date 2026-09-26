//! HTTP-level tests of the OpenAI-compatible provider against a simulated API.

use pretty_assertions::assert_eq;
use serde_json::json;
use vibe_core::{
    CompletionRequest, ContentBlock, ErrorKind, Message, ModelProvider, ProviderConfig, StopReason,
    ToolSpec,
};
use vibe_providers::{OpenAiCompatibleProvider, RetryPolicy};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer, attempts: u32) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new("sk-test-openai-key")
        .with_base_url(format!("{}/v1", server.uri()))
        .with_retry_policy(RetryPolicy::immediate(attempts))
}

fn request() -> CompletionRequest {
    let mut req = CompletionRequest::new("gpt-5", vec![Message::user("Read main.rs")]);
    req.system = "You are a coder.".into();
    req.tools = vec![ToolSpec {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object"}),
    }];
    req
}

fn ok_body() -> serde_json::Value {
    json!({
        "id": "chatcmpl-1",
        "model": "gpt-5",
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "done"}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1}
    })
}

#[tokio::test]
async fn success_with_tool_use() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test-openai-key"))
        .and(body_partial_json(json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "You are a coder."},
                {"role": "user", "content": "Read main.rs"}
            ],
            "tools": [{"type": "function", "function": {"name": "read_file"}}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1",
            "model": "gpt-5",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"}}]
                }
            }],
            "usage": {"prompt_tokens": 40, "completion_tokens": 12}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider(&server, 3).complete(request()).await.unwrap();
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(
        response.message.content,
        vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: json!({"path": "main.rs"}),
        }]
    );
    assert_eq!(response.usage.input_tokens, 40);
    assert_eq!(response.usage.output_tokens, 12);
}

#[tokio::test]
async fn rate_limited_then_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(
                    json!({"error": {"message": "Rate limit reached", "type": "requests"}}),
                ),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    let response = provider(&server, 3).complete(request()).await.unwrap();
    assert_eq!(response.message.text(), "done");
}

#[tokio::test]
async fn insufficient_quota_429_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "You exceeded your current quota", "code": "insufficient_quota"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server, 4).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
}

#[tokio::test]
async fn unauthorized_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "Incorrect API key provided: sk-test-openai-key"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server, 4).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthFailed);
    assert!(
        !err.message.contains("sk-test-openai-key"),
        "{}",
        err.message
    );
}

#[tokio::test]
async fn server_errors_exhaust_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
        .expect(4)
        .mount(&server)
        .await;
    let err = provider(&server, 4).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
}

#[tokio::test]
async fn malformed_json_is_a_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>gateway</html>"))
        .mount(&server)
        .await;
    let err = provider(&server, 1).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);

    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices": []})))
        .mount(&server)
        .await;
    let err = provider(&server, 1).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
}

#[tokio::test]
async fn context_length_is_classified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {"message": "This model's maximum context length is 128000 tokens.", "code": "context_length_exceeded"}
        })))
        .mount(&server)
        .await;
    let err = provider(&server, 3).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ContextTooLong);
}

#[tokio::test]
async fn local_server_without_key_sends_no_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            json!({"model": "qwen2.5-coder", "max_tokens": 8192}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = ProviderConfig {
        kind: "openai".into(),
        base_url: Some(format!("{}/v1", server.uri())),
        default_model: Some("qwen2.5-coder".into()),
        ..Default::default()
    };
    let provider = OpenAiCompatibleProvider::from_config("ollama", &cfg)
        .unwrap()
        .with_retry_policy(RetryPolicy::none());
    let mut req = request();
    req.model.clear();
    provider.complete(req).await.unwrap();

    let received = server.received_requests().await.unwrap();
    assert!(received[0].headers.get("authorization").is_none());
}
