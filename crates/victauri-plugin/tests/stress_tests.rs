mod common;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use victauri_core::{CommandRegistry, EventLog, EventRecorder};
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;
use victauri_plugin::mcp::{build_app, build_app_full, build_app_with_options};
use victauri_plugin::privacy::PrivacyConfig;

use common::SimpleMockBridge;

// ── Callback Mock Bridge ───────────────────────────────────────────────────

type ResponseFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct CallbackMockBridge {
    windows: Vec<victauri_core::WindowState>,
    pending_evals: victauri_plugin::PendingCallbacks,
    response_fn: ResponseFn,
}

impl CallbackMockBridge {
    fn new(
        labels: &[&str],
        pending_evals: victauri_plugin::PendingCallbacks,
        response_fn: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            windows: labels
                .iter()
                .map(|label| victauri_core::WindowState {
                    label: label.to_string(),
                    title: format!("{label} Window"),
                    url: "http://localhost/".to_string(),
                    visible: true,
                    focused: labels.first() == Some(label),
                    maximized: false,
                    minimized: false,
                    fullscreen: false,
                    position: (100, 100),
                    size: (800, 600),
                })
                .collect(),
            pending_evals,
            response_fn: Arc::new(response_fn),
        }
    }
}

fn extract_eval_id(script: &str) -> Option<String> {
    let marker = r#"id: ""#;
    let start = script.find(marker)? + marker.len();
    let end = start + 36;
    if end <= script.len() {
        Some(script[start..end].to_string())
    } else {
        None
    }
}

impl WebviewBridge for CallbackMockBridge {
    fn eval_webview(&self, _label: Option<&str>, script: &str) -> Result<(), String> {
        // Skip the parse-watchdog companion script (no `__victauri_ok` marker); only the
        // real eval wrapper produces a result, mirroring a real webview.
        if !script.contains("__victauri_ok") {
            return Ok(());
        }
        if let Some(id) = extract_eval_id(script) {
            let response = (self.response_fn)(script);
            let pending = self.pending_evals.clone();
            std::thread::spawn(move || {
                let mut map = pending.blocking_lock();
                if let Some(tx) = map.remove(&id) {
                    let _ = tx.send(response);
                }
            });
        }
        Ok(())
    }

    fn get_window_states(&self, label: Option<&str>) -> Vec<victauri_core::WindowState> {
        match label {
            Some(l) => self
                .windows
                .iter()
                .filter(|w| w.label == l)
                .cloned()
                .collect(),
            None => self.windows.clone(),
        }
    }

    fn list_window_labels(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.label.clone()).collect()
    }

    fn get_native_handle(&self, _label: Option<&str>) -> Result<isize, String> {
        Err("native handle not available in mock".to_string())
    }

    fn manage_window(&self, _label: Option<&str>, action: &str) -> Result<String, String> {
        Ok(format!("{action} executed"))
    }

    fn resize_window(&self, _label: Option<&str>, _width: u32, _height: u32) -> Result<(), String> {
        Ok(())
    }

    fn move_window(&self, _label: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
        Ok(())
    }

    fn set_window_title(&self, _label: Option<&str>, _title: &str) -> Result<(), String> {
        Ok(())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn test_state() -> Arc<VictauriState> {
    Arc::new(VictauriState {
        event_log: EventLog::new(1000),
        registry: CommandRegistry::new(),
        port: std::sync::atomic::AtomicU16::new(0),
        pending_evals: Arc::new(Mutex::new(HashMap::new())),
        recorder: EventRecorder::new(1000),
        privacy: PrivacyConfig::default(),
        eval_timeout: std::time::Duration::from_secs(30),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        started_at: std::time::Instant::now(),
        tool_invocations: std::sync::atomic::AtomicU64::new(0),
        allow_file_navigation: false,
        command_timings: victauri_plugin::introspection::CommandTimings::new(),
        fault_registry: victauri_plugin::introspection::FaultRegistry::new(),
        contract_store: victauri_plugin::introspection::ContractStore::new(),
        startup_timeline: victauri_plugin::introspection::StartupTimeline::new(),
        event_bus: victauri_plugin::introspection::EventBusMonitor::default(),
        task_tracker: victauri_plugin::introspection::TaskTracker::new(),
        bridge_ready: std::sync::atomic::AtomicBool::new(true),
        bridge_notify: tokio::sync::Notify::new(),
        db_search_paths: Vec::new(),
        screencast: std::sync::Arc::new(victauri_plugin::screencast::Screencast::default()),
    })
}

async fn start_test_server(state: Arc<VictauriState>, labels: &[&str]) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app(state, bridge);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_callback_server(
    state: Arc<VictauriState>,
    labels: &[&str],
    response_fn: impl Fn(&str) -> String + Send + Sync + 'static,
) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(CallbackMockBridge::new(
        labels,
        state.pending_evals.clone(),
        response_fn,
    ));
    let app = build_app(state, bridge);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_auth_server(state: Arc<VictauriState>, labels: &[&str], token: &str) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app_with_options(state, bridge, Some(token.to_string()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn mcp_session(base: &str) -> (reqwest::Client, String) {
    let client = reqwest::Client::new();
    let init_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "stress-test", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();

    let session_id = init_resp
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .send()
        .await
        .unwrap();

    (client, session_id)
}

async fn call_tool(
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> String {
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        }))
        .send()
        .await
        .unwrap();
    resp.text().await.unwrap()
}

// ── Sequential Load ───────────────────────────────────────────────────────

#[tokio::test]
async fn sequential_100_tool_calls() {
    let state = test_state();
    let base = start_test_server(state.clone(), &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    for i in 0..100 {
        let body = call_tool(
            &client,
            &base,
            &sid,
            "window",
            serde_json::json!({"action": "list"}),
        )
        .await;
        assert!(body.contains("main"), "call {i} should succeed: {body}");
    }

    let invocations = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        invocations >= 100,
        "at least 100 invocations: got {invocations}"
    );
}

// ── Concurrent Sessions ─────────────────────────────────────────────────���─

#[tokio::test]
async fn concurrent_5_sessions() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;

    let mut handles = Vec::new();
    for _ in 0..5 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let (client, sid) = mcp_session(&url).await;
            let body = call_tool(
                &client,
                &url,
                &sid,
                "window",
                serde_json::json!({"action": "list"}),
            )
            .await;
            assert!(body.contains("main"), "session should work: {body}");
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent Tool Calls ─────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_20_tool_calls() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;

    let client = reqwest::Client::new();
    let mut handles = Vec::new();
    for i in 0..20 {
        let c = client.clone();
        let u = base.clone();
        handles.push(tokio::spawn(async move {
            let resp = c.get(format!("{u}/health")).send().await.unwrap();
            assert!(
                resp.status().is_success() || resp.status().as_u16() == 429,
                "concurrent request {i} should not crash: {}",
                resp.status()
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Concurrent Eval Calls ─────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_10_eval_calls() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let mut handles = Vec::new();
    for i in 0..10 {
        let c = client.clone();
        let u = base.clone();
        let s = sid.clone();
        handles.push(tokio::spawn(async move {
            let body = call_tool(
                &c,
                &u,
                &s,
                "eval_js",
                serde_json::json!({"code": format!("'result_{i}'")}),
            )
            .await;
            assert!(
                !body.contains("\"isError\":true"),
                "eval {i} should not error: {body}"
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

// ── Recording Stress ──────────────────────────────────────────────────────

#[tokio::test]
async fn recording_100_checkpoints() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "start"}),
    )
    .await;

    for i in 0..100 {
        let body = call_tool(
            &client,
            &base,
            &sid,
            "recording",
            serde_json::json!({"action": "checkpoint", "checkpoint_id": format!("cp-{i}")}),
        )
        .await;
        assert!(body.contains("created"), "checkpoint {i}: {body}");
    }

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "list_checkpoints"}),
    )
    .await;
    assert!(
        body.contains("cp-99"),
        "should have last checkpoint: {body}"
    );
    assert!(
        body.contains("cp-0"),
        "should have first checkpoint: {body}"
    );

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
}

// ── URL Validation ────────────────────────────────────────────────────────

#[tokio::test]
async fn navigate_blocks_javascript_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "javascript:alert(1)"}),
    )
    .await;
    assert!(
        body.contains("error")
            || body.contains("invalid")
            || body.contains("blocked")
            || body.contains("javascript"),
        "javascript: should be blocked: {body}"
    );
}

#[tokio::test]
async fn navigate_blocks_data_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "data:text/html,<script>alert(1)</script>"}),
    )
    .await;
    assert!(
        body.contains("error")
            || body.contains("invalid")
            || body.contains("blocked")
            || body.contains("data"),
        "data: should be blocked: {body}"
    );
}

#[tokio::test]
async fn navigate_allows_https_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("navigate(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "https://example.com"}),
    )
    .await;
    assert!(body.contains("ok"), "https should be allowed: {body}");
}

#[tokio::test]
async fn navigate_allows_http_localhost() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("navigate(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "http://localhost:4444"}),
    )
    .await;
    assert!(
        body.contains("ok"),
        "http localhost should be allowed: {body}"
    );
}

// ── CSS Color Sanitization ────────────────────────────────────────────────

#[tokio::test]
async fn highlight_rejects_malicious_color() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| r#"{"ok":true}"#.to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "inspect",
        serde_json::json!({
            "action": "highlight",
            "ref_id": "e1",
            "color": "red; background-image: url(evil.js)"
        }),
    )
    .await;
    assert!(
        body.contains("error") || body.contains("invalid"),
        "malicious color should be rejected: {body}"
    );
}

#[tokio::test]
async fn highlight_accepts_valid_rgba() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("highlightElement(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "inspect",
        serde_json::json!({
            "action": "highlight",
            "ref_id": "e1",
            "color": "rgba(0, 128, 255, 0.5)"
        }),
    )
    .await;
    assert!(body.contains("ok"), "valid rgba should work: {body}");
}

// ── Auth Brute Force ──────────────────────────────────────────────────────

#[tokio::test]
async fn auth_brute_force_all_rejected() {
    let state = test_state();
    let base = start_auth_server(state, &["main"], "correct-secret-token").await;
    let client = reqwest::Client::new();

    let wrong_tokens = [
        "wrong1",
        "wrong2",
        "wrong3",
        "wrong4",
        "wrong5",
        "CORRECT-SECRET-TOKEN",
        "correct-secret-toke",
        "correct-secret-tokenn",
    ];

    for token in &wrong_tokens {
        let resp = client
            .get(format!("{base}/info"))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            401,
            "token '{token}' should be rejected"
        );
    }
}

#[tokio::test]
async fn auth_correct_token_accepted() {
    let state = test_state();
    let base = start_auth_server(state, &["main"], "my-secret").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/info"))
        .header("Authorization", "Bearer my-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "correct token should work");
}

// ── Unicode / Emoji Handling ──────────────────────────────────────────────

#[tokio::test]
async fn unicode_in_eval_js() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"unicode ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        serde_json::json!({"code": "'Hello \\u4e16\\u754c \\u{1F600}'"}),
    )
    .await;
    assert!(
        body.contains("unicode ok"),
        "unicode eval should work: {body}"
    );
}

#[tokio::test]
async fn unicode_in_window_title() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "set_title", "title": "Hello World"}),
    )
    .await;
    assert!(body.contains("ok"), "unicode title should work: {body}");
}

#[tokio::test]
async fn emoji_in_fill_value() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("fill(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "input",
        serde_json::json!({"action": "fill", "ref_id": "e1", "value": "test value here"}),
    )
    .await;
    assert!(body.contains("ok"), "emoji fill should work: {body}");
}

// ── Large Payloads ────────────────────────────────────────────────────────

#[tokio::test]
async fn large_eval_result() {
    let state = test_state();
    let large_response = "x".repeat(100_000);
    let expected_fragment = &large_response[..20];
    let response_clone = large_response.clone();
    let base =
        start_callback_server(state, &["main"], move |_| format!("\"{response_clone}\"")).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        serde_json::json!({"code": "'large string'"}),
    )
    .await;
    assert!(
        body.contains(expected_fragment),
        "large result should be returned"
    );
}

#[tokio::test]
async fn large_css_injection() {
    let state = test_state();
    let large_css = format!("body {{ {} }}", "color: red; ".repeat(1000));
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("injectCss(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "css",
        serde_json::json!({"action": "inject", "css": large_css}),
    )
    .await;
    assert!(body.contains("ok"), "large CSS should be accepted: {body}");
}

// ── Strict Privacy Mode ───────────────────────────────────────────────────

#[tokio::test]
async fn strict_privacy_blocks_dangerous_tools() {
    let config = victauri_plugin::privacy::strict_privacy_config();
    let state = Arc::new(VictauriState {
        event_log: EventLog::new(1000),
        registry: CommandRegistry::new(),
        port: std::sync::atomic::AtomicU16::new(0),
        pending_evals: Arc::new(Mutex::new(HashMap::new())),
        recorder: EventRecorder::new(1000),
        privacy: config,
        eval_timeout: std::time::Duration::from_secs(30),
        shutdown_tx: tokio::sync::watch::channel(false).0,
        started_at: std::time::Instant::now(),
        tool_invocations: std::sync::atomic::AtomicU64::new(0),
        allow_file_navigation: false,
        command_timings: victauri_plugin::introspection::CommandTimings::new(),
        fault_registry: victauri_plugin::introspection::FaultRegistry::new(),
        contract_store: victauri_plugin::introspection::ContractStore::new(),
        startup_timeline: victauri_plugin::introspection::StartupTimeline::new(),
        event_bus: victauri_plugin::introspection::EventBusMonitor::default(),
        task_tracker: victauri_plugin::introspection::TaskTracker::new(),
        bridge_ready: std::sync::atomic::AtomicBool::new(true),
        bridge_notify: tokio::sync::Notify::new(),
        db_search_paths: Vec::new(),
        screencast: std::sync::Arc::new(victauri_plugin::screencast::Screencast::default()),
    });
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        serde_json::json!({"code": "1+1"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "eval_js disabled in strict mode: {body}"
    );

    let body = call_tool(&client, &base, &sid, "screenshot", serde_json::json!({})).await;
    assert!(
        body.contains("disabled"),
        "screenshot disabled in strict mode: {body}"
    );

    // Read-only tools should still work
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "list"}),
    )
    .await;
    assert!(
        body.contains("main"),
        "read-only tools should work in strict mode: {body}"
    );
}

// ── Rate Limiter ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rate_limiter_eventually_rejects() {
    let state = test_state();
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let limiter = Arc::new(victauri_plugin::auth::RateLimiterState::new(50));
    let app = build_app_full(state, bridge, None, Some(limiter));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let mut got_429 = false;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..500 {
        let c = client.clone();
        let u = format!("{base}/info");
        tasks.spawn(async move { c.get(&u).send().await.unwrap().status() });
    }
    while let Some(result) = tasks.join_next().await {
        if result.unwrap() == 429 {
            got_429 = true;
            break;
        }
    }
    assert!(got_429, "rate limiter should trigger on burst");
}

// ── Double Recording Start ────────────────────────────────────────────────

#[tokio::test]
async fn recording_double_start_returns_error() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "start"}),
    )
    .await;
    assert!(body.contains("started"), "first start: {body}");

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "start"}),
    )
    .await;
    assert!(
        body.contains("error") || body.contains("already"),
        "double start should error: {body}"
    );

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
}

// ── Stop Without Start ────────────────────────────────────────────────────

#[tokio::test]
async fn recording_stop_without_start() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
    assert!(
        body.contains("error") || body.contains("no recording"),
        "stop without start should error: {body}"
    );
}
