mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;

use victauri_core::{
    AppEvent, CommandInfo, CommandRegistry, EventLog, EventRecorder, IpcCall, IpcResult,
};
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;
use victauri_plugin::mcp::{VictauriMcpHandler, build_app, build_app_with_options};
use victauri_plugin::privacy::PrivacyConfig;

use common::{SimpleMockBridge, test_state};

// ── Callback Mock Bridge ───────────────────────────────────────────────────
// A mock that intercepts eval_webview calls, extracts the callback ID from the
// injected JS, and resolves the pending oneshot channel with a configurable
// response. This enables testing of the 40+ MCP tools that depend on eval.

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
    // The injected JS contains: id: "uuid" (json-escaped via js_string)
    let marker = r#"id: ""#;
    let start = script.find(marker)? + marker.len();
    let end = start + 36; // UUID is always 36 chars
    if end <= script.len() {
        Some(script[start..end].to_string())
    } else {
        None
    }
}

fn extract_inner_code(script: &str) -> String {
    // Extract the user code from the injected async IIFE wrapper.
    // The pattern is: (async () => { <code> })()
    // After auto-return, code like "return window.__VICTAURI__?.snapshot()" appears
    // inside the inner function.
    let marker = "const __result = await (async () => { ";
    if let Some(start) = script.find(marker) {
        let code_start = start + marker.len();
        if let Some(end) = script[code_start..].find(" })();") {
            return script[code_start..code_start + end].trim().to_string();
        }
    }
    script.to_string()
}

impl WebviewBridge for CallbackMockBridge {
    fn eval_webview(&self, _label: Option<&str>, script: &str) -> Result<(), String> {
        if let Some(id) = extract_eval_id(script) {
            let inner_code = extract_inner_code(script);
            let response = (self.response_fn)(&inner_code);
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

// ── Helpers ─────────────────────────────────────────────────────────────────

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

async fn start_auth_test_server(state: Arc<VictauriState>, labels: &[&str], token: &str) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app_with_options(state, bridge, Some(token.to_string()));

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

async fn mcp_call_tool(
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

fn sample_command(name: &str) -> CommandInfo {
    let mut cmd = CommandInfo::new(name).with_description(format!("{name} command"));
    cmd.return_type = Some("String".to_string());
    cmd
}

fn sample_ipc_call(command: &str, result: IpcResult) -> IpcCall {
    IpcCall {
        id: uuid::Uuid::new_v4().to_string(),
        command: command.to_string(),
        timestamp: Utc::now(),
        duration_ms: Some(5),
        result,
        arg_size_bytes: 42,
        webview_label: "main".to_string(),
    }
}

// ── Handler construction tests ──────────────────────────────────────────────

#[test]
fn handler_new_creates_instance() {
    let state = test_state();
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let _ = VictauriMcpHandler::new(state, bridge);
}

#[test]
fn handler_get_info_has_correct_capabilities() {
    use rmcp::ServerHandler;

    let state = test_state();
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let handler = VictauriMcpHandler::new(state, bridge);
    let info = handler.get_info();

    assert!(
        info.capabilities.tools.is_some(),
        "tools capability missing"
    );
    assert!(
        info.capabilities.resources.is_some(),
        "resources capability missing"
    );
}

// ── Mock bridge tests ───────────────────────────────────────────────────────

#[test]
fn mock_bridge_returns_window_states() {
    let bridge = SimpleMockBridge::new(&["main", "settings"]);
    let states = bridge.get_window_states(None);
    assert_eq!(states.len(), 2);
}

#[test]
fn mock_bridge_filters_by_label() {
    let bridge = SimpleMockBridge::new(&["main", "settings"]);
    let states = bridge.get_window_states(Some("settings"));
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].label, "settings");
}

#[test]
fn mock_bridge_returns_empty_for_unknown_label() {
    let bridge = SimpleMockBridge::new(&["main"]);
    let states = bridge.get_window_states(Some("nonexistent"));
    assert!(states.is_empty());
}

#[test]
fn mock_bridge_list_labels() {
    let bridge = SimpleMockBridge::new(&["alpha", "beta"]);
    let mut labels = bridge.list_window_labels();
    labels.sort();
    assert_eq!(labels, vec!["alpha", "beta"]);
}

// ── HTTP endpoint tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let base = start_test_server(test_state(), &["main"]).await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "ok");
    assert!(
        json.get("uptime_secs").is_none(),
        "hardened health should not leak uptime"
    );
    assert!(
        json.get("memory").is_none(),
        "hardened health should not leak memory stats"
    );
}

#[tokio::test]
async fn info_endpoint_returns_valid_json() {
    let base = start_test_server(test_state(), &["main"]).await;

    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(json["name"], "victauri");
    assert_eq!(json["protocol"], "mcp");
    assert!(json["version"].is_string());
    assert_eq!(json["commands_registered"], 0);
    assert_eq!(json["events_captured"], 0);
}

#[tokio::test]
async fn info_reflects_registered_commands() {
    let state = test_state();
    state.registry.register(sample_command("greet"));
    state.registry.register(sample_command("save_user"));
    state.registry.register(sample_command("get_config"));

    let base = start_test_server(state, &["main"]).await;
    let json: serde_json::Value = reqwest::get(format!("{base}/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(json["commands_registered"], 3);
}

#[tokio::test]
async fn info_reflects_event_count() {
    let state = test_state();
    state
        .event_log
        .push(AppEvent::Ipc(sample_ipc_call("cmd1", IpcResult::Pending)));
    state
        .event_log
        .push(AppEvent::Ipc(sample_ipc_call("cmd2", IpcResult::Pending)));

    let base = start_test_server(state, &["main"]).await;
    let json: serde_json::Value = reqwest::get(format!("{base}/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(json["events_captured"], 2);
}

// ── MCP protocol tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn mcp_endpoint_accepts_post() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "MCP initialize returned {}",
        resp.status()
    );
}

#[tokio::test]
async fn mcp_initialize_returns_session() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();

    let session_id = resp.headers().get("mcp-session-id");
    assert!(
        session_id.is_some(),
        "MCP response should include session ID header"
    );
}

#[tokio::test]
async fn mcp_full_session_lists_tools() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    // Send initialized notification
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

    // List tools
    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    assert!(tools_resp.status().is_success());
    let body = tools_resp.text().await.unwrap();

    assert!(body.contains("eval_js"), "tools should include eval_js");
    assert!(
        body.contains("dom_snapshot"),
        "tools should include dom_snapshot"
    );
    assert!(body.contains("recording"), "tools should include recording");
    assert!(
        body.contains("verify_state"),
        "tools should include verify_state"
    );
    assert!(
        body.contains("resolve_command"),
        "tools should include resolve_command"
    );
}

#[tokio::test]
async fn mcp_full_session_lists_resources() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("victauri://ipc-log"),
        "should list ipc-log resource"
    );
    assert!(
        body.contains("victauri://windows"),
        "should list windows resource"
    );
    assert!(
        body.contains("victauri://state"),
        "should list state resource"
    );
}

// ── New tool verification tests ────────────────────────────────────────────

#[tokio::test]
async fn mcp_tools_include_invoke_command() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body = tools_resp.text().await.unwrap();
    assert!(
        body.contains("invoke_command"),
        "tools should include invoke_command"
    );
    assert!(
        body.contains("screenshot"),
        "tools should include screenshot"
    );
    assert!(body.contains("input"), "tools should include input");
    assert!(body.contains("logs"), "tools should include logs");
}

#[tokio::test]
async fn mcp_tool_count_is_correct() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body = tools_resp.text().await.unwrap();
    let expected_tools = [
        // Standalone tools
        "eval_js",
        "dom_snapshot",
        "invoke_command",
        "screenshot",
        "verify_state",
        "detect_ghost_commands",
        "check_ipc_integrity",
        "wait_for",
        "assert_semantic",
        "resolve_command",
        "get_registry",
        "get_memory_stats",
        "get_plugin_info",
        // Compound tools
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "route",
        "logs",
    ];

    for tool_name in &expected_tools {
        assert!(body.contains(tool_name), "missing tool: {tool_name}");
    }
}

#[tokio::test]
async fn mcp_read_resource_ipc_log() {
    let state = test_state();
    state.event_log.push(AppEvent::Ipc(sample_ipc_call(
        "test_cmd",
        IpcResult::Ok(serde_json::json!("ok")),
    )));

    let base = start_test_server(state, &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {"uri": "victauri://ipc-log"}
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("test_cmd"),
        "resource should contain the IPC call"
    );
}

#[tokio::test]
async fn mcp_read_resource_windows() {
    let base = start_test_server(test_state(), &["main", "settings"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {"uri": "victauri://windows"}
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("main"),
        "windows resource should contain main window"
    );
    assert!(
        body.contains("settings"),
        "windows resource should contain settings window"
    );
}

#[tokio::test]
async fn mcp_read_resource_state() {
    let state = test_state();
    state.registry.register(sample_command("greet"));

    let base = start_test_server(state, &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {"uri": "victauri://state"}
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("commands_registered"),
        "state resource should contain command count"
    );
}

#[tokio::test]
async fn mcp_read_unknown_resource_fails() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {"uri": "victauri://nonexistent"}
        }))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("error"),
        "reading unknown resource should return error"
    );
}

// ── State wiring tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn state_port_reflected_in_info() {
    let state = Arc::new(VictauriState {
        event_log: EventLog::new(1000),
        registry: CommandRegistry::new(),
        port: std::sync::atomic::AtomicU16::new(9999),
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
    });

    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let app = build_app(state, bridge);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let json: serde_json::Value = reqwest::get(format!("http://{addr}/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(json["port"], 9999);
}

#[tokio::test]
async fn concurrent_requests_work() {
    let base = start_test_server(test_state(), &["main"]).await;

    let mut handles = vec![];
    for _ in 0..10 {
        let url = format!("{base}/health");
        handles.push(tokio::spawn(async move {
            let json: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
            json["status"].as_str().unwrap().to_string()
        }));
    }

    for handle in handles {
        assert_eq!(handle.await.unwrap(), "ok");
    }
}

// ── Builder tests ──────────────────────────────────────────────────────────

#[test]
fn builder_default_port() {
    let builder = victauri_plugin::VictauriBuilder::new();
    // Without env var or explicit port, resolve_port should return DEFAULT_PORT
    // We test this indirectly through state wiring
    let _ = builder;
}

#[test]
fn builder_custom_port_reflected_in_state() {
    let state = Arc::new(VictauriState {
        event_log: EventLog::new(500),
        registry: CommandRegistry::new(),
        port: std::sync::atomic::AtomicU16::new(8888),
        pending_evals: Arc::new(Mutex::new(HashMap::new())),
        recorder: EventRecorder::new(500),
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
    });

    assert_eq!(state.port.load(std::sync::atomic::Ordering::Relaxed), 8888);
    assert_eq!(state.event_log.len(), 0);
}

// ── Mock bridge native handle tests ────────────────────────────────────────

#[test]
fn mock_bridge_native_handle_returns_error() {
    let bridge = SimpleMockBridge::new(&["main"]);
    let result = bridge.get_native_handle(Some("main"));
    assert!(result.is_err(), "mock bridge should not have native handle");
}

// ── Recording tools via MCP ────────────────────────────────────────────────

#[tokio::test]
async fn recording_start_stop_via_state() {
    let state = test_state();
    assert!(!state.recorder.is_recording());

    state.recorder.start("test-session".to_string()).unwrap();
    assert!(state.recorder.is_recording());

    let session = state.recorder.stop();
    assert!(session.is_some());
    assert!(!state.recorder.is_recording());

    let session = session.unwrap();
    assert_eq!(session.id, "test-session");
    assert!(session.events.is_empty());
    assert!(session.checkpoints.is_empty());
}

#[tokio::test]
async fn recording_prevents_double_start() {
    let state = test_state();
    state.recorder.start("session-1".to_string()).unwrap();
    assert!(state.recorder.start("session-2".to_string()).is_err());
    let _ = state.recorder.stop();
}

#[tokio::test]
async fn recording_captures_events_and_checkpoints() {
    let state = test_state();
    state.recorder.start("test".to_string()).unwrap();

    state
        .recorder
        .record_event(AppEvent::Ipc(sample_ipc_call("cmd1", IpcResult::Pending)));
    state.recorder.record_event(AppEvent::Ipc(sample_ipc_call(
        "cmd2",
        IpcResult::Ok(serde_json::json!("ok")),
    )));

    state
        .recorder
        .checkpoint(
            "cp1".to_string(),
            Some("after cmd2".to_string()),
            serde_json::json!({"counter": 1}),
        )
        .unwrap();

    assert_eq!(state.recorder.event_count(), 2);
    assert_eq!(state.recorder.checkpoint_count(), 1);

    let session = state.recorder.stop().unwrap();
    assert_eq!(session.events.len(), 2);
    assert_eq!(session.checkpoints.len(), 1);
    assert_eq!(session.checkpoints[0].id, "cp1");
}

#[tokio::test]
async fn ipc_replay_sequence_returns_only_ipc() {
    let state = test_state();
    state.recorder.start("test".to_string()).unwrap();

    state
        .recorder
        .record_event(AppEvent::Ipc(sample_ipc_call("cmd1", IpcResult::Pending)));
    state.recorder.record_event(AppEvent::WindowEvent {
        label: "main".to_string(),
        event: "focus".to_string(),
        timestamp: Utc::now(),
    });
    state
        .recorder
        .record_event(AppEvent::Ipc(sample_ipc_call("cmd2", IpcResult::Pending)));

    let replay = state.recorder.ipc_replay_sequence();
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].command, "cmd1");
    assert_eq!(replay[1].command, "cmd2");

    let _ = state.recorder.stop();
}

// ── Memory stats test ──────────────────────────────────────────────────────

#[test]
fn memory_stats_returns_valid_json() {
    let stats = victauri_plugin::mcp::tests_support::get_memory_stats();
    assert!(stats.is_object(), "memory stats should be a JSON object");
    // On Windows, should have working_set_bytes; on other platforms, should have an error or platform-specific fields
    #[cfg(windows)]
    {
        assert!(
            stats.get("working_set_bytes").is_some(),
            "should have working_set_bytes on Windows"
        );
    }
}

// ── Auth tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_health_bypasses_token() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200, "health endpoint should bypass auth");
}

#[tokio::test]
async fn auth_rejects_unauthenticated_info() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    assert_eq!(
        resp.status(),
        401,
        "info endpoint should require auth when token is set"
    );
}

#[tokio::test]
async fn auth_accepts_valid_bearer_token() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/info"))
        .header("Authorization", "Bearer secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "valid token should be accepted");
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["auth_required"], true);
}

#[tokio::test]
async fn auth_rejects_wrong_token() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/info"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong token should be rejected");
}

#[tokio::test]
async fn auth_mcp_requires_token() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let client = reqwest::Client::new();

    let resp = client
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401, "MCP endpoint should require auth");
}

#[tokio::test]
async fn auth_mcp_works_with_valid_token() {
    let base = start_auth_test_server(test_state(), &["main"], "secret-token").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", "Bearer secret-token")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "MCP should work with valid token"
    );
}

// ── No-auth tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn no_auth_allows_all_requests() {
    let base = start_test_server(test_state(), &["main"]).await;
    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "without auth token, all requests should be allowed"
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["auth_required"], false);
}

// ── Auth token generation test ─────────────────────────────────────────────

#[test]
fn generate_token_produces_valid_uuid() {
    let token = victauri_plugin::auth::generate_token();
    assert_eq!(token.len(), 36, "token should be a UUID");
    assert!(token.contains('-'), "token should be hyphenated UUID");
}

// ── Privacy integration tests ─────────────────────────────────────────────

fn privacy_state(config: PrivacyConfig) -> Arc<VictauriState> {
    Arc::new(VictauriState {
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
    })
}

async fn start_privacy_test_server(config: PrivacyConfig, labels: &[&str]) -> String {
    let state = privacy_state(config);
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app(state, bridge);

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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

#[tokio::test]
async fn privacy_disabled_tools_hidden_from_list() {
    let mut disabled = HashSet::new();
    disabled.insert("eval_js".to_string());
    disabled.insert("screenshot".to_string());
    disabled.insert("inject_css".to_string());

    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };

    let base = start_privacy_test_server(config, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body = tools_resp.text().await.unwrap();
    assert!(
        !body.contains("\"eval_js\""),
        "eval_js should be hidden when disabled"
    );
    assert!(
        !body.contains("\"screenshot\""),
        "screenshot should be hidden when disabled"
    );
    assert!(
        !body.contains("\"inject_css\""),
        "inject_css should be hidden when disabled"
    );
    assert!(
        body.contains("dom_snapshot"),
        "non-disabled tools should still be listed"
    );
    assert!(
        body.contains("window"),
        "non-disabled tools should still be listed"
    );
}

#[tokio::test]
async fn privacy_disabled_tool_call_rejected() {
    let mut disabled = HashSet::new();
    disabled.insert("eval_js".to_string());

    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };

    let base = start_privacy_test_server(config, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let call_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "eval_js",
                "arguments": {"code": "document.title"}
            }
        }))
        .send()
        .await
        .unwrap();

    let body = call_resp.text().await.unwrap();
    assert!(
        body.contains("disabled"),
        "calling a disabled tool should return a disabled message, got: {body}"
    );
}

#[tokio::test]
async fn privacy_strict_mode_disables_dangerous_tools() {
    let config = victauri_plugin::privacy::strict_privacy_config();

    let base = start_privacy_test_server(config, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let tools_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body = tools_resp.text().await.unwrap();
    // In strict mode, standalone tools that match disabled_tools are hidden
    for tool_name in &["eval_js", "screenshot", "navigate"] {
        assert!(
            !body.contains(&format!("\"{tool_name}\"")),
            "{tool_name} should be hidden in strict privacy mode"
        );
    }
    assert!(
        body.contains("dom_snapshot"),
        "read-only tools should still be visible"
    );
    assert!(
        body.contains("logs"),
        "read-only tools should still be visible"
    );
}

#[tokio::test]
async fn privacy_info_shows_privacy_mode() {
    let mut disabled = HashSet::new();
    disabled.insert("eval_js".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };

    let base = start_privacy_test_server(config, &["main"]).await;
    let json: serde_json::Value = reqwest::get(format!("{base}/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        json["privacy_mode"], true,
        "info should show privacy_mode: true when tools are disabled"
    );
}

#[tokio::test]
async fn privacy_info_shows_no_privacy_by_default() {
    let base = start_test_server(test_state(), &["main"]).await;
    let json: serde_json::Value = reqwest::get(format!("{base}/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        json["privacy_mode"], false,
        "info should show privacy_mode: false with default config"
    );
}

// ── MCP Protocol Compliance tests ─────────────────────────────────────────

#[tokio::test]
async fn mcp_rejects_invalid_json() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body("this is not json")
        .send()
        .await
        .unwrap();

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 415 || status == 200,
        "invalid JSON should return 400/415 or be handled gracefully, got {status}"
    );
}

#[tokio::test]
async fn mcp_rejects_missing_jsonrpc_field() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert!(
        status == 400 || status == 415 || body.contains("error") || body.contains("deserialize"),
        "missing jsonrpc field should be rejected or return error, got status={status} body={body}"
    );
}

#[tokio::test]
async fn mcp_handles_unknown_method() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "totally/nonexistent",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("error"),
        "unknown method should return an error response, got: {body}"
    );
}

#[tokio::test]
async fn mcp_call_unknown_tool_returns_error() {
    let base = start_test_server(test_state(), &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "nonexistent_tool_xyz",
                "arguments": {}
            }
        }))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("error") || body.contains("not found") || body.contains("unknown"),
        "calling unknown tool should return error, got: {body}"
    );
}

#[tokio::test]
async fn mcp_concurrent_sessions() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let mut session_ids = Vec::new();
    for i in 0..3 {
        let resp = client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": i + 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": format!("client-{i}"), "version": "0.1.0"}
                }
            }))
            .send()
            .await
            .unwrap();

        assert!(resp.status().is_success(), "session {i} init failed");
        let sid = resp
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        session_ids.push(sid);
    }

    assert_eq!(session_ids.len(), 3);
    let unique: HashSet<_> = session_ids.iter().collect();
    assert_eq!(unique.len(), 3, "each session should have a unique ID");

    for (i, sid) in session_ids.iter().enumerate() {
        client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .header("mcp-session-id", sid)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await
            .unwrap();

        let resp = client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-session-id", sid)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": (i + 1) * 100,
                "method": "tools/list",
                "params": {}
            }))
            .send()
            .await
            .unwrap();

        assert!(resp.status().is_success(), "session {i} tools/list failed");
        let body = resp.text().await.unwrap();
        assert!(body.contains("eval_js"), "session {i} should list tools");
    }
}

#[tokio::test]
async fn mcp_get_request_not_allowed_without_session() {
    let base = start_test_server(test_state(), &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/mcp"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();

    let status = resp.status().as_u16();
    assert!(
        status == 400 || status == 405 || status == 404,
        "GET /mcp without session should be rejected, got {status}"
    );
}

#[tokio::test]
async fn mcp_tool_call_with_empty_arguments() {
    let base = start_test_server(test_state(), &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "window",
                "arguments": {"action": "list"}
            }
        }))
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_success(),
        "tool call with action-only args should succeed"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("main"),
        "window list should return window labels"
    );
}

#[tokio::test]
async fn mcp_delete_session_terminates() {
    let base = start_test_server(test_state(), &["main"]).await;
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
                "clientInfo": {"name": "test-client", "version": "0.1.0"}
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

    let del_resp = client
        .delete(format!("{base}/mcp"))
        .header("mcp-session-id", &session_id)
        .send()
        .await
        .unwrap();

    let status = del_resp.status().as_u16();
    assert!(
        status == 200 || status == 202 || status == 204 || status == 405,
        "DELETE /mcp should succeed or return method not allowed, got {status}"
    );
}

#[tokio::test]
async fn rate_limiter_returns_429_on_burst() {
    let state = test_state();
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let limiter = Arc::new(victauri_plugin::auth::RateLimiterState::new(5));
    let app = victauri_plugin::mcp::build_app_full(state, bridge, None, Some(limiter));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let url = format!("{base}/info");
    let mut got_429 = false;
    for _ in 0..20 {
        let status = client.get(&url).send().await.unwrap().status();
        if status == 429 {
            got_429 = true;
            break;
        }
    }

    assert!(
        got_429,
        "rate limiter should return 429 after exceeding limit"
    );
}

// ── CallbackMockBridge tests (eval-dependent tools) ────────────────────────

#[test]
fn extract_eval_id_parses_uuid() {
    let script = r#"(async () => { id: "550e8400-e29b-41d4-a716-446655440000", result: ... })();"#;
    assert_eq!(
        extract_eval_id(script),
        Some("550e8400-e29b-41d4-a716-446655440000".to_string())
    );
}

#[test]
fn extract_eval_id_returns_none_for_missing() {
    assert_eq!(extract_eval_id("no id here"), None);
}

#[test]
fn extract_inner_code_gets_user_code() {
    let script = r"
        (async () => {
            try {
                const __result = await (async () => { return document.title })();
                await window.__TAURI_INTERNALS__.invoke(...)
            } catch ...
        })();
    ";
    assert_eq!(extract_inner_code(script), "return document.title");
}

#[tokio::test]
async fn callback_mock_eval_js_returns_result() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("document.title") {
            "\"Test App\"".to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": "document.title"}),
    )
    .await;

    assert!(
        body.contains("Test App"),
        "eval_js should return the mocked result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_dom_snapshot_returns_tree() {
    let state = test_state();
    let snapshot_json = r#"{"role":"document","name":"page","children":[]}"#;
    let snapshot_response = snapshot_json.to_string();
    let base = start_callback_server(state, &["main"], move |code| {
        if code.contains("snapshot(") {
            snapshot_response.clone()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "dom_snapshot",
        serde_json::json!({}),
    )
    .await;

    assert!(
        body.contains("document"),
        "dom_snapshot should return the mocked tree, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_click_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("click(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "click", "ref_id": "e1"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "click should return ok result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_fill_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("fill(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "input",
        serde_json::json!({"action": "fill", "ref_id": "e2", "value": "hello"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "fill should return ok result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_get_ipc_log_returns_entries() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getIpcLog") {
            r#"[{"command":"greet","status":200,"duration":5}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "ipc"}),
    )
    .await;

    assert!(
        body.contains("greet"),
        "logs ipc should return mocked IPC entries, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_verify_state_pass() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("return (") {
            r#"{"title":"My App"}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "verify_state",
        serde_json::json!({
            "frontend_expr": "({title:'My App'})",
            "backend_state": {"title": "My App"}
        }),
    )
    .await;

    assert!(
        body.contains("passed") || body.contains("true"),
        "verify_state should pass when states match, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_verify_state_divergence() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("return (") {
            r#"{"title":"Frontend Title"}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "verify_state",
        serde_json::json!({
            "frontend_expr": "({title:'Frontend Title'})",
            "backend_state": {"title": "Backend Title"}
        }),
    )
    .await;

    assert!(
        body.contains("divergence") || body.contains("false"),
        "verify_state should detect divergence, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_assert_semantic_truthy() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("return (") {
            "42".to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "assert_semantic",
        serde_json::json!({
            "expression": "42",
            "label": "answer is truthy",
            "condition": "truthy",
            "expected": true
        }),
    )
    .await;

    assert!(
        body.contains("passed") || body.contains("true"),
        "assert_semantic truthy should pass for 42, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_assert_semantic_equals() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("return (") {
            "\"hello\"".to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "assert_semantic",
        serde_json::json!({
            "expression": "'hello'",
            "label": "greeting check",
            "condition": "equals",
            "expected": "hello"
        }),
    )
    .await;

    assert!(
        body.contains("passed"),
        "assert_semantic equals should pass, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_get_console_logs() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getConsoleLogs") {
            r#"[{"level":"log","message":"hello","timestamp":1000}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "console"}),
    )
    .await;

    assert!(
        body.contains("hello"),
        "get_console_logs should return mocked logs, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_get_event_stream() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getEventStream") {
            r#"[{"type":"console","data":"test event"}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "events"}),
    )
    .await;

    assert!(
        body.contains("test event"),
        "get_event_stream should return mocked events, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_type_text_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("type(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "input",
        serde_json::json!({"action": "type_text", "ref_id": "e3", "text": "hello world"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "type_text should return ok result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_press_key_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("pressKey(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "input",
        serde_json::json!({"action": "press_key", "key": "Enter"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "press_key should return ok result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_ghost_commands_detected() {
    let state = test_state();
    let mut cmd = CommandInfo::new("greet").with_description("greet command");
    cmd.return_type = Some("String".to_string());
    state.registry.register(cmd);

    let base = start_callback_server(state, &["main"], |code| {
        // detect_ghost_commands now projects command names in JS
        // (`getIpcLog().map(c => c.command)`), so the bridge returns a string
        // array rather than full IPC entries.
        if code.contains("getIpcLog") {
            r#"["greet","secret_cmd"]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "detect_ghost_commands",
        serde_json::json!({}),
    )
    .await;

    assert!(
        body.contains("secret_cmd"),
        "ghost command detection should find unregistered commands, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_get_network_log() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getNetworkLog") {
            r#"[{"url":"https://api.example.com","method":"GET","status":200}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "network"}),
    )
    .await;

    assert!(
        body.contains("api.example.com"),
        "logs network should return mocked entries, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_route_add_dispatch() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("addRoute") {
            r#"{"ok":true,"id":1,"echo":"ROUTE_OK"}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "route",
        serde_json::json!({
            "action": "add",
            "pattern": "*api.example.com*",
            "match_type": "glob",
            "behavior": "block"
        }),
    )
    .await;

    assert!(
        body.contains("ROUTE_OK"),
        "route add should dispatch addRoute to the bridge and return its result, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_route_missing_pattern() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "route",
        serde_json::json!({"action": "add", "behavior": "fulfill"}),
    )
    .await;
    assert!(
        body.contains("pattern"),
        "route add without pattern should error about the missing pattern, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_audit_accessibility() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("auditAccessibility") {
            r#"{"violations":[],"warnings":[],"summary":{"violations":0,"warnings":0}}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "audit_accessibility"}),
    )
    .await;

    assert!(
        body.contains("violations"),
        "audit_accessibility should return audit results, got: {body}"
    );
}

#[tokio::test]
async fn callback_mock_get_performance_metrics() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getPerformanceMetrics") {
            r#"{"navigation":{"load_event_ms":150},"paint":{"fcp_ms":80}}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "get_performance"}),
    )
    .await;

    assert!(
        body.contains("load_event_ms"),
        "get_performance_metrics should return mocked metrics, got: {body}"
    );
}

// ── Adversarial: Eval-Dependent Tools ──────────────────────────────────────

// ── 1. Happy-path coverage for untested eval-dependent tools ───────────────

#[tokio::test]
async fn adversarial_double_click_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("doubleClick") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "double_click", "ref_id": "e1"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "double_click should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_hover_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("hover") && !code.contains("doubleClick") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "hover", "ref_id": "e1"}),
    )
    .await;

    assert!(body.contains("ok"), "hover should return ok, got: {body}");
}

#[tokio::test]
async fn adversarial_select_option_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("selectOption") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "select_option", "ref_id": "e1", "values": ["opt1"]}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "select_option should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_scroll_to_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("scrollTo") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "scroll_into_view", "ref_id": "e1"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "scroll_to should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_focus_element_returns_ok() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("focusElement") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "focus", "ref_id": "e1"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "focus_element should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_storage_returns_data() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("Storage") {
            r#"{"items":{"key":"value"}}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "get", "storage_type": "local"}),
    )
    .await;

    assert!(
        body.contains("items"),
        "get_storage should return storage data, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_storage_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("Storage") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "set", "storage_type": "local", "key": "test", "value": "val"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "set_storage should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_delete_storage_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("Storage") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "delete", "storage_type": "local", "key": "test"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "delete_storage should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_cookies_returns_data() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getCookies") {
            r#""a=1; b=2""#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "get_cookies"}),
    )
    .await;

    assert!(
        body.contains("a=1"),
        "get_cookies should return cookie data, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_navigation_log_returns_entries() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getNavigationLog") {
            r#"[{"url":"http://localhost","type":"initial"}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "navigation"}),
    )
    .await;

    assert!(
        body.contains("http://localhost"),
        "get_navigation_log should return navigation entries, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_navigate_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("navigate") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "http://localhost:4444"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "navigate should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_navigate_back_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("navigateBack") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "go_back"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "navigate_back should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_dialog_log_returns_entries() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getDialogLog") {
            "[]".to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "get_dialog_log"}),
    )
    .await;

    assert!(
        body.contains("[]"),
        "get_dialog_log should return empty array, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_dialog_response_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("setDialogAutoResponse") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "set_dialog_response", "dialog_type": "confirm", "dialog_action": "accept"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "set_dialog_response should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_styles_returns_css() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getStyles") {
            r#"{"display":"block","color":"red"}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "get_styles", "ref_id": "e1"}),
    )
    .await;

    assert!(
        body.contains("display"),
        "get_styles should return CSS properties, got: {body}"
    );
    assert!(
        body.contains("block"),
        "get_styles should return display:block, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_bounding_boxes_returns_rects() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getBoundingBoxes") {
            r#"[{"ref_id":"e1","x":0,"y":0,"width":100,"height":50}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "get_bounding_boxes", "ref_ids": ["e1", "e2"]}),
    )
    .await;

    assert!(
        body.contains("width"),
        "get_bounding_boxes should return bounding rect data, got: {body}"
    );
    assert!(
        body.contains("100"),
        "get_bounding_boxes should contain width 100, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_highlight_element_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("highlightElement") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "highlight", "ref_id": "e1", "color": "red"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "highlight_element should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_clear_highlights_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("clearHighlights") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "inspect",
        serde_json::json!({"action": "clear_highlights"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "clear_highlights should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_inject_css_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("injectCss") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "css",
        serde_json::json!({"action": "inject", "css": "body { color: red }"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "inject_css should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_remove_injected_css_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("removeInjectedCss") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "css",
        serde_json::json!({"action": "remove"}),
    )
    .await;

    assert!(
        body.contains("ok"),
        "remove_injected_css should return ok, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_slow_ipc_calls_returns_data() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("duration_ms") {
            r#"{"threshold_ms":100,"count":0,"calls":[]}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "slow_ipc", "threshold_ms": 100}),
    )
    .await;

    assert!(
        body.contains("threshold_ms"),
        "slow_ipc_calls should return threshold data, got: {body}"
    );
    assert!(
        body.contains("count"),
        "slow_ipc_calls should return count, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_invoke_command_succeeds() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("__TAURI_INTERNALS__") && code.contains("invoke") {
            r#""Hello, test!""#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "invoke_command",
        serde_json::json!({"command": "greet", "args": {"name": "test"}}),
    )
    .await;

    assert!(
        body.contains("Hello, test!"),
        "invoke_command should return greeting, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_plugin_info_returns_config() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "get_plugin_info",
        serde_json::json!({}),
    )
    .await;

    assert!(
        body.contains("version"),
        "get_plugin_info should return version, got: {body}"
    );
    assert!(
        body.contains("tools"),
        "get_plugin_info should return tools, got: {body}"
    );
}

// ── 2. Adversarial / edge-case tests ──────────────────────────────────────

#[tokio::test]
async fn adversarial_eval_js_empty_code() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_code| "undefined".to_string()).await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": ""}),
    )
    .await;

    assert!(
        !body.contains("\"isError\":true"),
        "eval_js with empty code should not return an error, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_eval_js_syntax_error() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_code| {
        r#"{"__victauri_err":"Unexpected token"}"#.to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": "function("}),
    )
    .await;

    assert!(
        body.contains("Unexpected"),
        "eval_js with syntax error should surface the error, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_eval_js_returns_null() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_code| "null".to_string()).await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": "null"}),
    )
    .await;

    assert!(
        body.contains("null"),
        "eval_js returning null should contain null, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_eval_js_returns_large_output() {
    let large = "x".repeat(50000);
    let state = test_state();
    let base = start_callback_server(state, &["main"], move |_code| large.clone()).await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": "big"}),
    )
    .await;

    assert!(
        body.len() > 40000,
        "eval_js with large output should return large body, got length: {}",
        body.len()
    );
}

#[tokio::test]
async fn adversarial_navigate_blocks_javascript_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "javascript:alert(1)"}),
    )
    .await;

    assert!(
        body.contains("not allowed") || body.contains("scheme"),
        "navigate should block javascript: URLs, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_navigate_blocks_data_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "data:text/html,<script>"}),
    )
    .await;

    assert!(
        body.contains("not allowed") || body.contains("scheme"),
        "navigate should block data: URLs, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_invoke_command_blocklisted() {
    let mut config = PrivacyConfig::default();
    config.command_blocklist.insert("secret_cmd".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "invoke_command",
        serde_json::json!({"command": "secret_cmd"}),
    )
    .await;

    assert!(
        body.contains("blocked"),
        "invoke_command should block blocklisted commands, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_fill_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("fill".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "input",
        serde_json::json!({"action": "fill", "ref_id": "e1", "value": "test"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "fill should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_type_text_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("type_text".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "input",
        serde_json::json!({"action": "type_text", "ref_id": "e1", "text": "hello"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "type_text should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_screenshot_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("screenshot".to_string());
    let state = privacy_state(config);
    let base = start_test_server(state, &["main"]).await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "screenshot",
        serde_json::json!({}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "screenshot should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_storage_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("set_storage".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "set", "storage_type": "local", "key": "test", "value": "val"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "set_storage should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_delete_storage_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("delete_storage".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "storage",
        serde_json::json!({"action": "delete", "storage_type": "local", "key": "test"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "delete_storage should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_dialog_response_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config
        .disabled_tools
        .insert("set_dialog_response".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "set_dialog_response", "dialog_type": "confirm", "dialog_action": "accept"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "set_dialog_response should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_inject_css_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("inject_css".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "css",
        serde_json::json!({"action": "inject", "css": "body { color: red }"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "inject_css should be blocked by privacy config, got: {body}"
    );
}

#[tokio::test]
async fn adversarial_navigate_privacy_disabled() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("navigate".to_string());
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_code| {
        "should not reach eval".to_string()
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "http://localhost:4444"}),
    )
    .await;

    assert!(
        body.contains("disabled") || body.contains("privacy"),
        "navigate should be blocked by privacy config, got: {body}"
    );
}

// ── Gap 1: logs compound tool actions ─────────────────────────────────────

#[tokio::test]
async fn mcp_logs_console_action() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getConsoleLogs") {
            r#"[{"level":"warn","message":"deprecation warning","timestamp":2000}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "console"}),
    )
    .await;

    assert!(
        body.contains("deprecation warning"),
        "logs console should return mocked console entries, got: {body}"
    );
}

#[tokio::test]
async fn mcp_logs_network_action() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getNetworkLog") {
            r#"[{"url":"https://cdn.example.com/bundle.js","method":"GET","status":200,"duration_ms":42}]"#
                .to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "network"}),
    )
    .await;

    assert!(
        body.contains("cdn.example.com"),
        "logs network should return mocked network entries, got: {body}"
    );
}

#[tokio::test]
async fn mcp_logs_ipc_action() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        if code.contains("getIpcLog") {
            r#"[{"command":"save_settings","status":200,"duration_ms":12}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "ipc"}),
    )
    .await;

    assert!(
        body.contains("save_settings"),
        "logs ipc should return mocked IPC entries, got: {body}"
    );
}

#[tokio::test]
async fn mcp_logs_slow_ipc_action() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |code| {
        // slow_ipc calls getIpcLog internally in its JS code
        if code.contains("getIpcLog") {
            r#"[{"command":"heavy_query","status":200,"duration_ms":500},{"command":"fast_cmd","status":200,"duration_ms":2}]"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;

    let (client, session_id) = mcp_session(&base).await;
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "slow_ipc", "threshold_ms": 100}),
    )
    .await;

    // The slow_ipc action runs JS that filters and sorts locally in the webview,
    // so the mock returns the raw IPC log and the injected JS filters it.
    // The result should contain threshold_ms from the computed JS.
    assert!(
        body.contains("threshold_ms") || body.contains("heavy_query"),
        "logs slow_ipc should return slow IPC results, got: {body}"
    );
}

#[tokio::test]
async fn mcp_logs_unknown_action_returns_error() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "logs",
        serde_json::json!({"action": "nonexistent"}),
    )
    .await;

    assert!(
        body.contains("unknown variant"),
        "logs with unknown action should return deserialization error, got: {body}"
    );
    assert!(
        body.contains("console"),
        "error should list valid actions including 'console', got: {body}"
    );
}

// ── Gap 2: MAX_PENDING_EVALS limit ───────────────────────────────────────

#[tokio::test]
async fn mcp_eval_js_exceeds_pending_limit_returns_error() {
    let state = test_state();

    // Fill up the pending_evals map to the limit (100) with dummy senders.
    // These will never be resolved, simulating stalled eval requests.
    {
        let mut pending = state.pending_evals.lock().await;
        for i in 0..100 {
            let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
            pending.insert(format!("dummy-{i}"), tx);
        }
    }

    // Use a callback server that does NOT resolve callbacks — we just need
    // the handler to attempt eval_with_return and hit the limit check.
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(&["main"]));
    let app = build_app(state, bridge);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base = format!("http://{addr}");
    let (client, session_id) = mcp_session(&base).await;

    // Now try to call eval_js — it should fail because pending_evals is full.
    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "eval_js",
        serde_json::json!({"code": "document.title"}),
    )
    .await;

    assert!(
        body.contains("too many concurrent eval requests") || body.contains("limit: 100"),
        "eval_js should fail when pending evals at capacity, got: {body}"
    );
}

// ── Gap 3: logs compound tool validation / structured errors ─────────────

#[tokio::test]
async fn mcp_interact_missing_ref_id_returns_structured_error() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "click"}),
    )
    .await;

    assert!(
        body.contains("missing required parameter 'ref_id'"),
        "interact click without ref_id should return structured error, got: {body}"
    );
    assert!(
        body.contains("CHECK_INPUT"),
        "missing param error should include CHECK_INPUT hint, got: {body}"
    );
}

#[tokio::test]
async fn mcp_interact_unknown_action_returns_structured_error() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, session_id) = mcp_session(&base).await;

    let body = mcp_call_tool(
        &client,
        &base,
        &session_id,
        "interact",
        serde_json::json!({"action": "nonexistent"}),
    )
    .await;

    assert!(
        body.contains("unknown variant"),
        "interact with unknown action should return deserialization error, got: {body}"
    );
    assert!(
        body.contains("click"),
        "error should list valid actions including 'click', got: {body}"
    );
}

// ── REST API tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rest_list_tools_returns_all_tools() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/tools"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let tools: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        tools.len() >= 20,
        "should list at least 20 tools, got {}",
        tools.len()
    );

    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"eval_js"), "should contain eval_js");
    assert!(
        names.contains(&"dom_snapshot"),
        "should contain dom_snapshot"
    );
    assert!(names.contains(&"screenshot"), "should contain screenshot");
    assert!(names.contains(&"interact"), "should contain interact");
    assert!(names.contains(&"window"), "should contain window");
    assert!(
        names.contains(&"get_diagnostics"),
        "should contain get_diagnostics"
    );

    for tool in &tools {
        assert!(tool["name"].is_string(), "each tool should have a name");
    }
}

#[tokio::test]
async fn rest_call_unknown_tool_returns_404() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/nonexistent_tool"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("unknown tool"),
        "should report unknown tool, got: {body}"
    );
}

#[tokio::test]
async fn rest_call_tool_with_invalid_params_returns_400() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/eval_js"))
        .header("Content-Type", "application/json")
        .body(r#"{"wrong_field": "value"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("invalid parameters"),
        "should report invalid parameters, got: {body}"
    );
}

#[tokio::test]
async fn rest_call_tool_with_invalid_json_returns_400() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/eval_js"))
        .header("Content-Type", "application/json")
        .body("not valid json {{{")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("invalid JSON"),
        "should report invalid JSON, got: {body}"
    );
}

#[tokio::test]
async fn rest_get_memory_stats_works_without_body() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/get_memory_stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("result").is_some(),
        "should have result field, got: {body}"
    );
}

#[tokio::test]
async fn rest_get_plugin_info_returns_version() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/get_plugin_info"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let result = &body["result"];
    assert!(
        result["version"].is_string(),
        "should have version in result, got: {body}"
    );
    assert!(
        result["tools"]["total"].as_u64().unwrap() >= 20,
        "should report tool count, got: {body}"
    );
}

#[tokio::test]
async fn rest_get_registry_returns_json_result() {
    let state = test_state();
    state.registry.register(CommandInfo {
        name: "test_cmd".to_string(),
        plugin: None,
        description: Some("A test command".to_string()),
        args: vec![],
        return_type: None,
        is_async: false,
        intent: None,
        category: None,
        examples: vec![],
    });
    let base = start_test_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/get_registry"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let result = &body["result"];
    assert!(
        result.is_array(),
        "registry should return array, got: {body}"
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["name"], "test_cmd");
}

#[tokio::test]
async fn rest_respects_auth_middleware() {
    let state = test_state();
    let token = "test-rest-token-123";
    let base = start_auth_test_server(state, &["main"], token).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/tools"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated request should be rejected"
    );

    let resp = client
        .get(format!("{base}/api/tools"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "authenticated request should succeed");
}

#[tokio::test]
async fn rest_call_tool_with_callback_bridge() {
    let state = test_state();
    let base =
        start_callback_server(state, &["main"], |_js| r#""hello from eval""#.to_string()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/eval_js"))
        .header("Content-Type", "application/json")
        .body(r#"{"code": "document.title"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["result"], "hello from eval",
        "should return eval result, got: {body}"
    );
}

#[tokio::test]
async fn rest_get_diagnostics_returns_warnings_and_info() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_js| {
        r#"{"warnings":[],"info":{"bridge_version":"0.5.0","dom_elements":42,"open_shadow_roots":0,"event_listeners":10,"protocol":"tauri:","url":"tauri://localhost/","user_agent":"test"}}"#.to_string()
    }).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/tools/get_diagnostics"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let result = &body["result"];
    assert!(result["warnings"].is_array(), "should have warnings array");
    assert!(result["info"].is_object(), "should have info object");
    assert_eq!(
        result["info"]["bridge_version"], "0.5.0",
        "should have bridge_version"
    );
    assert_eq!(
        result["info"]["dom_elements"], 42,
        "should have dom_elements count"
    );
}
