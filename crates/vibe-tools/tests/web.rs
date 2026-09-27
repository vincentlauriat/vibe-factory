//! `web_fetch` and `web_search` against a local HTTP server.

use serde_json::json;
use vibe_core::{Permissions, Tool, ToolContext};
use vibe_tools::{WebFetchTool, WebSearchTool};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx(network: bool) -> ToolContext {
    let mut permissions = Permissions::read_only();
    permissions.network = network;
    ToolContext::new(".").with_permissions(permissions)
}

#[tokio::test]
async fn fetch_needs_network_and_refuses_internal_hosts() {
    let tool = WebFetchTool::new(Vec::new());
    let denied = tool
        .call(&ctx(false), json!({"url": "https://example.com"}))
        .await
        .unwrap();
    assert!(denied.is_error && denied.content.contains("Permission denied"));
    for url in [
        "http://127.0.0.1:9/",
        "http://localhost:9/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "file:///etc/passwd",
    ] {
        let out = tool.call(&ctx(true), json!({"url": url})).await.unwrap();
        assert!(out.is_error, "{url}: {}", out.content);
        assert!(out.content.starts_with("Refused"), "{url}: {}", out.content);
    }
    let limited = WebFetchTool::new(vec!["docs.rs".into()]);
    let out = limited
        .call(&ctx(true), json!({"url": "https://example.com/"}))
        .await
        .unwrap();
    assert!(out.content.contains("not in the allowed domains"));
}

#[tokio::test]
async fn fetch_converts_html_and_follows_checked_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/doc"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/doc"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<h1>Guide</h1><script>x()</script><p>Use &lt;T&gt;.</p>",
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/img"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(vec![0x89, b'P', b'N', b'G', 0, 0, 0]),
        )
        .mount(&server)
        .await;
    let tool = WebFetchTool::new(Vec::new()).allow_internal_addresses();
    let out = tool
        .call(&ctx(true), json!({"url": format!("{}/old", server.uri())}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.content, "Guide\n\nUse <T>.");
    assert!(out.metadata["url"].as_str().unwrap().ends_with("/doc"));
    assert_eq!(out.metadata["status"], 200);
    let img = tool
        .call(&ctx(true), json!({"url": format!("{}/img", server.uri())}))
        .await
        .unwrap();
    assert!(img.is_error && img.content.contains("binary"));
    // Without the test switch the local server is refused, redirect or not.
    let strict = WebFetchTool::new(Vec::new())
        .call(&ctx(true), json!({"url": format!("{}/old", server.uri())}))
        .await
        .unwrap();
    assert!(strict.content.starts_with("Refused"));
}

#[tokio::test]
async fn search_lists_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust sse"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
            {"title": "SSE in Rust", "url": "https://a.example/sse", "content": "How to\n stream"},
            {"title": "Other", "url": "https://b.example", "content": ""}
        ]})))
        .mount(&server)
        .await;
    let tool = WebSearchTool::new(format!("{}/search?q={{query}}&format=json", server.uri()));
    assert!(
        tool.call(&ctx(false), json!({"query": "x"}))
            .await
            .unwrap()
            .is_error
    );
    let out = tool
        .call(&ctx(true), json!({"query": "rust sse"}))
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content
            .starts_with("1. SSE in Rust\n   https://a.example/sse\n   How to stream"),
        "{}",
        out.content
    );
    assert!(out.content.contains("2. Other"));
}
