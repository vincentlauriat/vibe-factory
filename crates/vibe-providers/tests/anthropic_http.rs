//! HTTP-level tests of the Anthropic provider against a simulated API.

use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;
use vibe_core::{
    CompletionRequest, ContentBlock, ErrorKind, Message, ModelProvider, StopReason, ToolSpec,
};
use vibe_providers::{AnthropicProvider, RetryPolicy};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer, attempts: u32) -> AnthropicProvider {
    AnthropicProvider::new("sk-ant-test-key")
        .with_base_url(format!("{}/", server.uri()))
        .with_retry_policy(RetryPolicy::immediate(attempts))
}

fn request() -> CompletionRequest {
    let mut req = CompletionRequest::new("sonnet", vec![Message::user("List the files")]);
    req.system = "You are a coder.".into();
    req.tools = vec![ToolSpec {
        name: "list_dir".into(),
        description: "List a directory".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];
    req
}

fn ok_body() -> serde_json::Value {
    json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-5",
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "done"}],
        "usage": {"input_tokens": 3, "output_tokens": 1}
    })
}

#[tokio::test]
async fn success_with_tool_use() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(json!({
            "model": "claude-sonnet-5",
            "system": [{"type": "text", "text": "You are a coder.", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "list_dir", "input_schema": {"type": "object"}}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_01",
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "Let me look."},
                {"type": "tool_use", "id": "toolu_1", "name": "list_dir", "input": {"path": "."}}
            ],
            "usage": {"input_tokens": 120, "output_tokens": 30, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 0}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider(&server, 3).complete(request()).await.unwrap();
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.model, "claude-sonnet-5");
    assert_eq!(response.message.text(), "Let me look.");
    assert_eq!(
        response.message.content[1],
        ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "list_dir".into(),
            input: json!({"path": "."}),
        }
    );
    assert_eq!(response.usage.input_tokens, 120);
    assert_eq!(response.usage.cache_read_tokens, 100);
}

#[tokio::test]
async fn rate_limited_then_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}})),
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
    assert_eq!(response.stop_reason, StopReason::EndTurn);
}

#[tokio::test]
async fn rate_limit_exhaustion_keeps_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .expect(2)
        .mount(&server)
        .await;
    let err = provider(&server, 2).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::ZERO));
}

#[tokio::test]
async fn unauthorized_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server, 5).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::AuthFailed);
    assert!(err.message.contains("invalid x-api-key"));
}

#[tokio::test]
async fn billing_error_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "Your credit balance is too low"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server, 5).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
}

#[tokio::test]
async fn server_errors_exhaust_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .expect(3)
        .mount(&server)
        .await;
    let err = provider(&server, 3).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
    assert_eq!(err.message, "HTTP 500: internal error");
}

#[tokio::test]
async fn malformed_json_is_a_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;
    let err = provider(&server, 1).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
    assert!(err.message.contains("malformed JSON"));

    // Valid JSON with the wrong shape is reported the same way.
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"unexpected": true})))
        .mount(&server)
        .await;
    let err = provider(&server, 1).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
}

#[tokio::test]
async fn context_too_long_is_classified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "prompt is too long: 300000 tokens > 200000 maximum"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = provider(&server, 3).complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::ContextTooLong);
}

#[tokio::test]
async fn bearer_mode_and_custom_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer oauth-token"))
        .and(header("anthropic-beta", "oauth-2025-04-20"))
        .and(header("x-team", "core"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-team", "core".parse().unwrap());
    let provider = AnthropicProvider::with_auth(None)
        .with_bearer_token("oauth-token")
        .with_headers(headers)
        .with_base_url(server.uri())
        .with_retry_policy(RetryPolicy::none());
    provider.complete(request()).await.unwrap();

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert!(received[0].headers.get("x-api-key").is_none());
    let ua = received[0]
        .headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ua.starts_with("vibe-factory/"));
}

#[tokio::test]
async fn network_failure_is_classified() {
    // Port 9 (discard) on localhost is essentially never listening.
    let provider = AnthropicProvider::new("k")
        .with_base_url("http://127.0.0.1:9")
        .with_retry_policy(RetryPolicy::immediate(2));
    let err = provider.complete(request()).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
}

#[tokio::test]
async fn signed_thinking_is_replayed_on_the_next_tool_turn() {
    let server = MockServer::start().await;
    let signed =
        json!({"type": "thinking", "thinking": "I should list files", "signature": "sig-abc"});
    // Second turn: the assistant message must start with the signed block.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "medium"},
            "messages": [
                {"role": "user"},
                {"role": "assistant", "content": [signed.clone(), {"type": "tool_use", "id": "toolu_1"}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]}
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;
    // First turn: thinking plus a tool call.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "claude-sonnet-5",
            "stop_reason": "tool_use",
            "content": [signed, {"type": "tool_use", "id": "toolu_1", "name": "list_dir", "input": {"path": "."}}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&server, 1);
    let mut req = request();
    req.thinking_budget = Some(4096);
    let first = provider.complete(req.clone()).await.unwrap();
    assert_eq!(first.stop_reason, StopReason::ToolUse);
    assert_eq!(
        first.message.content[0],
        ContentBlock::Thinking {
            text: "I should list files".into()
        }
    );

    req.messages.push(first.message);
    req.messages
        .push(Message::tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: "Cargo.toml".into(),
            is_error: false,
        }]));
    let second = provider.complete(req).await.unwrap();
    assert_eq!(second.message.text(), "done");
}

fn sse(events: &[serde_json::Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}

fn collect_deltas() -> (
    std::sync::Arc<std::sync::Mutex<Vec<vibe_core::StreamDelta>>>,
    impl Fn(vibe_core::StreamDelta) + Send + Sync,
) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = {
        let seen = seen.clone();
        move |d| seen.lock().unwrap().push(d)
    };
    (seen, sink)
}

#[tokio::test]
async fn streaming_rebuilds_text_thinking_and_tool_use() {
    let server = MockServer::start().await;
    let body = sse(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-sonnet-5",
               "content": [], "usage": {"input_tokens": 50, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Hmm"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Let me "}}),
        json!({"type": "ping"}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "look."}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "list_dir", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"path\": "}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\".\"}"}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 42}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({"stream": true})))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (seen, sink) = collect_deltas();
    let response = provider(&server, 3)
        .complete_streaming(request(), &sink)
        .await
        .unwrap();
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.usage.input_tokens, 50);
    assert_eq!(response.usage.output_tokens, 42);
    assert_eq!(
        response.message.content,
        vec![
            ContentBlock::Thinking { text: "Hmm".into() },
            ContentBlock::Text {
                text: "Let me look.".into()
            },
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "list_dir".into(),
                input: json!({"path": "."})
            },
        ]
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        vec![
            vibe_core::StreamDelta::Thinking { text: "Hmm".into() },
            vibe_core::StreamDelta::Text {
                text: "Let me ".into()
            },
            vibe_core::StreamDelta::Text {
                text: "look.".into()
            },
        ]
    );
}

#[tokio::test]
async fn streaming_retries_before_output_but_not_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_string("overloaded"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let complete = sse(&[
        json!({"type": "message_start", "message": {"model": "m", "content": [], "usage": {"input_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(complete))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // A stream that ends without message_stop, after text was sent.
    let cut = sse(&[
        json!({"type": "message_start", "message": {"model": "m", "content": [], "usage": {"input_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
    ]);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(cut))
        .mount(&server)
        .await;

    let p = provider(&server, 3);
    let (seen, sink) = collect_deltas();
    let first = p.complete_streaming(request(), &sink).await.unwrap();
    assert_eq!(first.message.text(), "ok");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the failed attempt sent nothing"
    );

    let (seen, sink) = collect_deltas();
    let err = p.complete_streaming(request(), &sink).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Other);
    assert!(err.message.contains("after output started"), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 1, "not retried after output");
}

#[tokio::test]
async fn streaming_error_event_is_classified() {
    let server = MockServer::start().await;
    let body = sse(&[
        json!({"type": "message_start", "message": {"model": "m", "content": [], "usage": {}}}),
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    ]);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(2)
        .mount(&server)
        .await;
    let (_, sink) = collect_deltas();
    let err = provider(&server, 2)
        .complete_streaming(request(), &sink)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ServerError);
    assert!(err.message.contains("Overloaded"));
}
