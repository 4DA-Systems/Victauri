//! End-to-end integration tests for victauri-browser.
//!
//! Tests the full pipeline: HTTP server (actual TCP listener) → MCP handler →
//! bridge dispatch → native messaging encoding. Uses `reqwest` for real HTTP calls
//! against a server bound to a random available port for test isolation.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use victauri_browser::auth::RateLimiterState;
use victauri_browser::bridge_dispatch::BridgeDispatch;
use victauri_browser::mcp_handler::VictauriBrowserHandler;
use victauri_browser::native_messaging;
use victauri_browser::server::{build_app, build_app_full};
use victauri_browser::tab_state::TabManager;

// ---------------------------------------------------------------------------
// Test infrastructure
// ---------------------------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    tab_manager: Arc<TabManager>,
    dispatch: Arc<BridgeDispatch>,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TestServer {
    /// Start a test server on a random available port with optional auth token.
    async fn start(auth_token: Option<String>) -> Self {
        Self::start_with_rate_limit(auth_token, None).await
    }

    /// Start a test server with a custom rate limiter.
    async fn start_with_rate_limit(
        auth_token: Option<String>,
        rate_limiter: Option<Arc<RateLimiterState>>,
    ) -> Self {
        let tab_manager = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_manager), Arc::clone(&dispatch));

        let app = match rate_limiter {
            Some(limiter) => build_app_full(handler, auth_token, Some(limiter)),
            None => build_app(handler, auth_token),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        // Give the server a moment to start accepting connections.
        tokio::time::sleep(Duration::from_millis(10)).await;

        Self {
            addr,
            tab_manager,
            dispatch,
            _shutdown: shutdown_tx,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

// ===========================================================================
// 1. Server startup and health
// ===========================================================================

#[tokio::test]
async fn health_returns_ok() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn info_returns_correct_json() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/info")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "victauri-browser");
    assert_eq!(json["protocol"], "mcp");
    assert_eq!(json["mode"], "browser");
    assert_eq!(json["tabs"], 0);
    assert_eq!(json["auth_required"], false);
}

#[tokio::test]
async fn info_reflects_auth_required_when_set() {
    let server = TestServer::start(Some("my-token".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "Bearer my-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["auth_required"], true);
}

#[tokio::test]
async fn info_reflects_tab_count() {
    let server = TestServer::start(None).await;
    server
        .tab_manager
        .on_tab_created(1, "https://example.com", "Example")
        .await;
    server
        .tab_manager
        .on_tab_created(2, "https://rust-lang.org", "Rust")
        .await;

    let resp = client().get(server.url("/info")).send().await.unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["tabs"], 2);
}

#[tokio::test]
async fn nonexistent_path_returns_404() {
    let server = TestServer::start(None).await;
    let resp = client()
        .get(server.url("/nonexistent"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ===========================================================================
// 2. Tool listing
// ===========================================================================

#[tokio::test]
async fn tool_listing_returns_20_tools() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/api/tools")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let tools = json.as_array().unwrap();
    assert_eq!(tools.len(), 20);
}

#[tokio::test]
async fn tool_listing_contains_expected_tools() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/api/tools")).send().await.unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();

    let expected = [
        "eval_js",
        "dom_snapshot",
        "find_elements",
        "interact",
        "input",
        "inspect",
        "css",
        "logs",
        "storage",
        "navigate",
        "wait_for",
        "assert_semantic",
        "recording",
        "screenshot",
        "tabs",
        "page_info",
        "cookies",
        "get_diagnostics",
        "get_plugin_info",
        "get_memory_stats",
    ];
    for name in expected {
        assert!(names.contains(&name), "missing tool: {name}");
    }
}

#[tokio::test]
async fn each_tool_has_name_and_description() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/api/tools")).send().await.unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();
    for tool in json.as_array().unwrap() {
        assert!(tool["name"].is_string(), "tool missing name");
        assert!(tool["description"].is_string(), "tool missing description");
    }
}

// ===========================================================================
// 3. Auth enforcement
// ===========================================================================

#[tokio::test]
async fn auth_rejects_request_without_token() {
    let server = TestServer::start(Some("secret-token".to_string())).await;
    let resp = client().get(server.url("/info")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_rejects_request_with_wrong_token() {
    let server = TestServer::start(Some("correct-token".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_accepts_correct_token() {
    let server = TestServer::start(Some("correct-token".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "Bearer correct-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn auth_case_insensitive_bearer_prefix() {
    let server = TestServer::start(Some("my-token".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "BEARER my-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn health_bypasses_auth() {
    let server = TestServer::start(Some("secret".to_string())).await;
    let resp = client().get(server.url("/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn auth_rejects_no_bearer_prefix() {
    let server = TestServer::start(Some("tok".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "tok")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_rejects_empty_bearer() {
    let server = TestServer::start(Some("secret".to_string())).await;
    let resp = client()
        .get(server.url("/info"))
        .header("authorization", "Bearer ")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_required_for_tool_execution() {
    let token = "my-secret";
    let server = TestServer::start(Some(token.to_string())).await;

    // Without token
    let resp = client()
        .post(server.url("/api/tools/get_plugin_info"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With correct token
    let resp = client()
        .post(server.url("/api/tools/get_plugin_info"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ===========================================================================
// 4. Rate limiting
// ===========================================================================

#[tokio::test]
async fn rate_limit_allows_within_budget() {
    let limiter = Arc::new(RateLimiterState::new(10));
    let server = TestServer::start_with_rate_limit(None, Some(limiter)).await;
    let http = client();

    // All 10 should succeed
    for _ in 0..10 {
        let resp = http.get(server.url("/info")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}

#[tokio::test]
async fn rate_limit_returns_429_when_exceeded() {
    let limiter = Arc::new(RateLimiterState::new(3));
    let server = TestServer::start_with_rate_limit(None, Some(limiter)).await;
    let http = client();

    // Use up the budget
    for _ in 0..3 {
        let resp = http.get(server.url("/info")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Next request should be rate limited
    let resp = http.get(server.url("/info")).send().await.unwrap();
    assert_eq!(resp.status(), 429);
}

#[tokio::test]
async fn rate_limit_concurrent_burst() {
    let limiter = Arc::new(RateLimiterState::new(5));
    let server = TestServer::start_with_rate_limit(None, Some(limiter)).await;

    let mut handles = vec![];
    for _ in 0..20 {
        let url = server.url("/info");
        handles.push(tokio::spawn(async move {
            let resp = client().get(&url).send().await.unwrap();
            resp.status().as_u16()
        }));
    }

    let mut ok_count = 0u32;
    let mut limited_count = 0u32;
    for h in handles {
        match h.await.unwrap() {
            200 => ok_count += 1,
            429 => limited_count += 1,
            s => panic!("unexpected status: {s}"),
        }
    }

    assert!(ok_count >= 1, "at least one request should pass");
    assert!(ok_count <= 5, "at most 5 should pass, got {ok_count}");
    assert!(limited_count >= 15, "not enough limited: {limited_count}");
}

// ===========================================================================
// 5. MCP protocol
// ===========================================================================

#[tokio::test]
async fn mcp_initialize_returns_valid_response() {
    let server = TestServer::start(None).await;
    let http = client();

    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "test-client",
                "version": "0.1.0"
            }
        }
    });

    // MCP Streamable HTTP requires Accept: application/json, text/event-stream
    let resp = http
        .post(server.url("/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&init_msg)
        .send()
        .await
        .unwrap();

    // MCP Streamable HTTP returns 200 with SSE or JSON-RPC body, or 202 (accepted)
    assert!(
        resp.status() == 200 || resp.status() == 202,
        "expected 200 or 202, got {}",
        resp.status()
    );

    if resp.status() == 200 {
        let text = resp.text().await.unwrap();
        // Could be SSE or JSON-RPC — verify it's not empty
        assert!(!text.is_empty(), "MCP response body should not be empty");
    }
}

#[tokio::test]
async fn mcp_endpoint_rejects_get_without_accept_header() {
    let server = TestServer::start(None).await;
    let resp = client().get(server.url("/mcp")).send().await.unwrap();
    // MCP Streamable HTTP GET without proper Accept header returns 406 Not Acceptable,
    // or with proper headers it returns SSE stream. Without headers, it should not return 200.
    let status = resp.status().as_u16();
    assert!(
        status == 405 || status == 406 || status == 400,
        "unexpected status for GET /mcp without Accept header: {status}"
    );
}

#[tokio::test]
async fn mcp_endpoint_post_without_accept_returns_406() {
    let server = TestServer::start(None).await;
    // POST to /mcp without the required Accept header
    let resp = client()
        .post(server.url("/mcp"))
        .header("content-type", "application/json")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
        .send()
        .await
        .unwrap();
    // rmcp requires Accept: application/json, text/event-stream
    let status = resp.status().as_u16();
    assert_eq!(
        status, 406,
        "expected 406 without Accept header, got {status}"
    );
}

// ===========================================================================
// 6. Tool execution without bridge (host-local tools)
// ===========================================================================

#[tokio::test]
async fn get_plugin_info_returns_metadata() {
    let server = TestServer::start(None).await;
    let resp = client()
        .post(server.url("/api/tools/get_plugin_info"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"]["name"], "victauri-browser");
    assert_eq!(json["result"]["mode"], "browser");
    assert_eq!(json["result"]["tool_count"], 20);
}

#[tokio::test]
async fn tabs_list_empty_when_no_tabs() {
    let server = TestServer::start(None).await;
    let resp = client()
        .post(server.url("/api/tools/tabs"))
        .json(&json!({"action": "list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["result"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn tabs_list_reflects_tab_state() {
    let server = TestServer::start(None).await;
    server
        .tab_manager
        .on_tab_created(1, "https://github.com", "GitHub")
        .await;
    server
        .tab_manager
        .on_tab_created(2, "https://crates.io", "crates.io")
        .await;
    server.tab_manager.on_tab_activated(2).await;

    let resp = client()
        .post(server.url("/api/tools/tabs"))
        .json(&json!({"action": "list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    let tabs = json["result"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);

    let active: Vec<_> = tabs.iter().filter(|t| t["active"] == true).collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["tab_id"], 2);
    assert_eq!(active[0]["url"], "https://crates.io");
}

#[tokio::test]
async fn unknown_tool_returns_error_response() {
    let server = TestServer::start(None).await;
    let resp = client()
        .post(server.url("/api/tools/nonexistent_tool"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("unknown tool"));
    assert!(json.get("result").is_none());
}

#[tokio::test]
async fn plugin_info_invocation_counter_increments() {
    let server = TestServer::start(None).await;
    let http = client();

    let resp = http
        .post(server.url("/api/tools/get_plugin_info"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"]["invocations"], 1);

    let resp = http
        .post(server.url("/api/tools/get_plugin_info"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"]["invocations"], 2);
}

// ===========================================================================
// 7. Native message encoding/decoding
// ===========================================================================

#[tokio::test]
async fn native_message_roundtrip_simple() {
    let msg = json!({"type": "execute", "id": "abc-123", "method": "snapshot", "args": {}});
    let mut buf = Vec::new();
    native_messaging::write_message(&mut buf, &msg)
        .await
        .unwrap();

    let mut reader = tokio::io::BufReader::new(buf.as_slice());
    let decoded = native_messaging::read_message(&mut reader).await.unwrap();
    assert_eq!(decoded, msg);
}

#[tokio::test]
async fn native_message_roundtrip_large_payload() {
    let big_data = "x".repeat(500_000);
    let msg = json!({"type": "response", "id": "big-1", "data": {"dom": big_data}});
    let mut buf = Vec::new();
    native_messaging::write_message(&mut buf, &msg)
        .await
        .unwrap();

    let mut reader = tokio::io::BufReader::new(buf.as_slice());
    let decoded = native_messaging::read_message(&mut reader).await.unwrap();
    assert_eq!(decoded["data"]["dom"].as_str().unwrap().len(), 500_000);
}

#[tokio::test]
async fn native_message_length_prefix_correct() {
    let msg = json!({"id": "test-1", "method": "eval"});
    let expected_bytes = serde_json::to_vec(&msg).unwrap();

    let mut buf = Vec::new();
    native_messaging::write_message(&mut buf, &msg)
        .await
        .unwrap();

    // First 4 bytes are LE u32 length
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    assert_eq!(len, expected_bytes.len());
    assert_eq!(&buf[4..], &expected_bytes);
}

#[tokio::test]
async fn native_message_multiple_sequential() {
    let messages = vec![
        json!({"type": "tab_created", "tab_id": 1, "url": "https://a.com", "title": "A"}),
        json!({"type": "response", "id": "cmd-1", "data": {"ok": true}}),
        json!({"type": "tab_activated", "tab_id": 1}),
        json!({"type": "response", "id": "cmd-2", "data": {"snapshot": "tree"}}),
    ];

    let mut buf = Vec::new();
    for msg in &messages {
        native_messaging::write_message(&mut buf, msg)
            .await
            .unwrap();
    }

    let mut reader = tokio::io::BufReader::new(buf.as_slice());
    for expected in &messages {
        let decoded = native_messaging::read_message(&mut reader).await.unwrap();
        assert_eq!(&decoded, expected);
    }

    // After all messages, should get disconnect error
    let eof = native_messaging::read_message(&mut reader).await;
    assert!(eof.is_err());
}

#[tokio::test]
async fn native_message_unicode_roundtrip() {
    let msg = json!({
        "type": "response",
        "id": "unicode-1",
        "data": {
            "title": "日本語ページ 🎉",
            "content": "Ñoño — em dash — «quotes»",
            "emoji": "👨‍👩‍👧‍👦"
        }
    });
    let mut buf = Vec::new();
    native_messaging::write_message(&mut buf, &msg)
        .await
        .unwrap();

    let mut reader = tokio::io::BufReader::new(buf.as_slice());
    let decoded = native_messaging::read_message(&mut reader).await.unwrap();
    assert_eq!(decoded["data"]["title"], "日本語ページ 🎉");
    assert_eq!(decoded["data"]["emoji"], "👨‍👩‍👧‍👦");
}

#[tokio::test]
async fn native_message_rejects_oversized_input() {
    let too_big = (1_048_576 + 1) as u32; // MAX_INPUT_SIZE + 1
    let mut buf = Vec::new();
    buf.extend_from_slice(&too_big.to_le_bytes());
    buf.extend(vec![0u8; 100]); // Doesn't matter, should reject at length check

    let mut reader = tokio::io::BufReader::new(buf.as_slice());
    let result = native_messaging::read_message(&mut reader).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn native_message_empty_stream_is_disconnect() {
    let buf: &[u8] = &[];
    let mut reader = tokio::io::BufReader::new(buf);
    let result = native_messaging::read_message(&mut reader).await;
    assert!(result.is_err());
}

// ===========================================================================
// 8. Bridge dispatch timeout
// ===========================================================================

#[tokio::test]
async fn bridge_dispatch_timeout_on_unresolved_command() {
    // Create a dispatch that writes to a Vec (we won't read from it).
    // The dispatch uses a 30s timeout internally, but we can test via the
    // register_test_pending mechanism + manual timeout.
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    // Register a pending command that will never get resolved
    let rx = dispatch.register_test_pending("will-timeout").await;

    // Use a short timeout to test the concept without waiting 30s
    let result = tokio::time::timeout(Duration::from_millis(100), rx).await;

    // Should timeout since no one resolved the command
    assert!(result.is_err(), "expected timeout, got a response");

    // Verify the pending command is still tracked
    assert_eq!(dispatch.pending_count().await, 1);
}

#[tokio::test]
async fn bridge_dispatch_cancel_all_resolves_pending_with_error() {
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    let rx1 = dispatch.register_test_pending("cmd-1").await;
    let rx2 = dispatch.register_test_pending("cmd-2").await;
    let rx3 = dispatch.register_test_pending("cmd-3").await;

    assert_eq!(dispatch.pending_count().await, 3);

    dispatch.cancel_all().await;

    assert_eq!(dispatch.pending_count().await, 0);

    // All receivers should get an error response
    let r1 = rx1.await.unwrap();
    assert!(r1.error.is_some());
    assert!(r1.error.unwrap().contains("disconnected"));

    let r2 = rx2.await.unwrap();
    assert!(r2.error.is_some());

    let r3 = rx3.await.unwrap();
    assert!(r3.error.is_some());
}

#[tokio::test]
async fn bridge_dispatch_response_resolves_correctly() {
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    let rx = dispatch.register_test_pending("my-cmd").await;

    dispatch
        .on_response(
            "my-cmd",
            Some(json!({"tag": "body", "children": [{"ref": "e0"}]})),
            None,
        )
        .await;

    let result = rx.await.unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.data.unwrap()["tag"], "body");
}

#[tokio::test]
async fn bridge_dispatch_error_response_propagates() {
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    let rx = dispatch.register_test_pending("err-cmd").await;

    dispatch
        .on_response("err-cmd", None, Some("element not found".to_string()))
        .await;

    let result = rx.await.unwrap();
    assert!(result.data.is_none());
    assert_eq!(result.error.unwrap(), "element not found");
}

// ===========================================================================
// Full pipeline: HTTP → handler → bridge dispatch → response
// ===========================================================================

#[tokio::test]
async fn full_pipeline_http_tool_call_with_mock_bridge_response() {
    let server = TestServer::start(None).await;

    // Set up a tab so tools that need a tab will work
    server
        .tab_manager
        .on_tab_created(1, "https://app.example.com", "App")
        .await;
    server.tab_manager.on_tab_activated(1).await;
    server.tab_manager.on_bridge_ready(1).await;

    // Spawn a background task that simulates the Chrome extension responding
    let dispatch = Arc::clone(&server.dispatch);
    let responder = tokio::spawn(async move {
        // Poll for pending commands and resolve them
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let ids = dispatch.pending_ids().await;
            if !ids.is_empty() {
                for id in ids {
                    dispatch
                        .on_response(
                            &id,
                            Some(json!({
                                "tag": "html",
                                "children": [
                                    {"tag": "body", "ref": "e0", "children": [
                                        {"tag": "div", "ref": "e1", "text": "Hello"}
                                    ]}
                                ]
                            })),
                            None,
                        )
                        .await;
                }
                return;
            }
        }
    });

    // Make an HTTP tool call that dispatches to the bridge
    let resp = client()
        .post(server.url("/api/tools/dom_snapshot"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"]["tag"], "html");
    assert_eq!(json["result"]["children"][0]["tag"], "body");
    assert_eq!(
        json["result"]["children"][0]["children"][0]["text"],
        "Hello"
    );

    responder.await.unwrap();
}

#[tokio::test]
async fn full_pipeline_eval_js_with_mock_response() {
    let server = TestServer::start(None).await;

    server
        .tab_manager
        .on_tab_created(1, "https://example.com", "Example")
        .await;
    server.tab_manager.on_tab_activated(1).await;

    let dispatch = Arc::clone(&server.dispatch);
    let responder = tokio::spawn(async move {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let ids = dispatch.pending_ids().await;
            if !ids.is_empty() {
                for id in ids {
                    dispatch
                        .on_response(&id, Some(json!("Example Page Title")), None)
                        .await;
                }
                return;
            }
        }
    });

    let resp = client()
        .post(server.url("/api/tools/eval_js"))
        .json(&json!({"code": "document.title"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"], "Example Page Title");

    responder.await.unwrap();
}

#[tokio::test]
async fn full_pipeline_error_propagation_from_bridge() {
    let server = TestServer::start(None).await;

    server
        .tab_manager
        .on_tab_created(1, "https://app.com", "App")
        .await;
    server.tab_manager.on_tab_activated(1).await;

    let dispatch = Arc::clone(&server.dispatch);
    let responder = tokio::spawn(async move {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let ids = dispatch.pending_ids().await;
            if !ids.is_empty() {
                for id in ids {
                    dispatch
                        .on_response(&id, None, Some("ref e99 not found in DOM".to_string()))
                        .await;
                }
                return;
            }
        }
    });

    let resp = client()
        .post(server.url("/api/tools/interact"))
        .json(&json!({"action": "click", "ref_id": "e99"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("e99"));
    assert!(json.get("result").is_none());

    responder.await.unwrap();
}

#[tokio::test]
async fn full_pipeline_concurrent_tool_calls() {
    let server = TestServer::start(None).await;

    server
        .tab_manager
        .on_tab_created(1, "https://app.com", "App")
        .await;
    server.tab_manager.on_tab_activated(1).await;

    // Background responder that resolves all pending commands
    let dispatch = Arc::clone(&server.dispatch);
    let responder = tokio::spawn(async move {
        let mut resolved = 0;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let ids = dispatch.pending_ids().await;
            for id in ids {
                dispatch
                    .on_response(&id, Some(json!({"resolved": resolved})), None)
                    .await;
                resolved += 1;
            }
            if resolved >= 10 {
                return;
            }
        }
    });

    // Launch 10 concurrent HTTP tool calls
    let mut handles = vec![];
    for _ in 0..10 {
        let url = server.url("/api/tools/dom_snapshot");
        handles.push(tokio::spawn(async move {
            let resp = client().post(&url).json(&json!({})).send().await.unwrap();
            resp.status().as_u16()
        }));
    }

    let mut successes = 0;
    for h in handles {
        if h.await.unwrap() == 200 {
            successes += 1;
        }
    }
    assert_eq!(successes, 10);

    responder.await.unwrap();
}

#[tokio::test]
async fn full_pipeline_tool_that_needs_extension_without_tabs_errors() {
    let server = TestServer::start(None).await;
    // No tabs registered — tools that dispatch to bridge will time out or error

    // eval_js with no code should return a parameter error (before dispatch)
    let resp = client()
        .post(server.url("/api/tools/eval_js"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["error"].as_str().unwrap().contains("code"));
}

// ===========================================================================
// Additional: Security headers and origin guard via real HTTP
// ===========================================================================

#[tokio::test]
async fn security_headers_present_on_all_responses() {
    let server = TestServer::start(None).await;

    for path in ["/health", "/info", "/api/tools"] {
        let resp = client().get(server.url(path)).send().await.unwrap();
        let headers = resp.headers();
        assert_eq!(
            headers.get("x-content-type-options").unwrap(),
            "nosniff",
            "missing x-content-type-options on {path}"
        );
        assert_eq!(
            headers.get("cache-control").unwrap(),
            "no-store",
            "missing cache-control on {path}"
        );
    }
}

#[tokio::test]
async fn origin_guard_blocks_external_origin() {
    let server = TestServer::start(None).await;
    let resp = client()
        .get(server.url("/health"))
        .header("origin", "https://evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn origin_guard_allows_localhost() {
    let server = TestServer::start(None).await;
    let resp = client()
        .get(server.url("/health"))
        .header("origin", format!("http://127.0.0.1:{}", server.addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn origin_guard_blocks_subdomain_bypass() {
    let server = TestServer::start(None).await;
    let resp = client()
        .get(server.url("/health"))
        .header("origin", "https://localhost.evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

// ===========================================================================
// Edge cases and robustness
// ===========================================================================

#[tokio::test]
async fn malformed_json_body_returns_400() {
    let server = TestServer::start(None).await;
    let resp = client()
        .post(server.url("/api/tools/eval_js"))
        .header("content-type", "application/json")
        .body("not valid json {{{")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn oversized_body_returns_413() {
    let server = TestServer::start(None).await;
    let huge = "x".repeat(3 * 1024 * 1024); // 3MB, over the 2MB limit
    let resp = client()
        .post(server.url("/api/tools/eval_js"))
        .header("content-type", "application/json")
        .body(huge)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
}

#[tokio::test]
async fn concurrent_health_checks_all_succeed() {
    let server = TestServer::start(None).await;

    let mut handles = vec![];
    for _ in 0..50 {
        let url = server.url("/health");
        handles.push(tokio::spawn(async move {
            let resp = client().get(&url).send().await.unwrap();
            resp.status().as_u16()
        }));
    }

    for h in handles {
        assert_eq!(h.await.unwrap(), 200);
    }
}

#[tokio::test]
async fn get_on_tool_endpoint_returns_405() {
    let server = TestServer::start(None).await;
    let resp = client()
        .get(server.url("/api/tools/eval_js"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

#[tokio::test]
async fn large_json_body_within_limit_accepted() {
    let server = TestServer::start(None).await;
    // 1.5MB — under the 2MB limit. Use get_plugin_info which doesn't dispatch.
    let padding = "x".repeat(1_500_000);
    let resp = client()
        .post(server.url("/api/tools/get_plugin_info"))
        .json(&json!({"unused": padding}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["result"]["name"], "victauri-browser");
}
