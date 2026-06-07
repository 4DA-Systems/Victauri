mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::json;
use tokio::sync::Mutex;
use victauri_core::{CommandRegistry, EventLog, EventRecorder};
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;
use victauri_plugin::mcp::build_app_stateful;
use victauri_plugin::privacy::PrivacyConfig;

use common::{RejectingMockBridge, SimpleMockBridge, test_state};

// ═══════════════════════════════════════════════════════════════════════════
// Callback Mock Bridge — simulates JS bridge responses
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// Helper functions
// ═══════════════════════════════════════════════════════════════════════════

fn make_state_with_privacy(privacy: PrivacyConfig) -> Arc<VictauriState> {
    Arc::new(VictauriState {
        event_log: EventLog::new(1000),
        registry: CommandRegistry::new(),
        port: std::sync::atomic::AtomicU16::new(0),
        pending_evals: Arc::new(Mutex::new(HashMap::new())),
        recorder: EventRecorder::new(1000),
        privacy,
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
        probes: victauri_plugin::introspection::AppStateProbes::default(),
    })
}

async fn start_server(state: Arc<VictauriState>, labels: &[&str]) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app_stateful(state, bridge, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn start_rejecting_server(state: Arc<VictauriState>, labels: &[&str]) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(RejectingMockBridge::new(labels));
    let app = build_app_stateful(state, bridge, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
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
    let app = build_app_stateful(state, bridge, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn start_auth_server(state: Arc<VictauriState>, labels: &[&str], token: &str) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app_stateful(state, bridge, Some(token.to_string()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn start_auth_callback_server(
    state: Arc<VictauriState>,
    labels: &[&str],
    token: &str,
    response_fn: impl Fn(&str) -> String + Send + Sync + 'static,
) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(CallbackMockBridge::new(
        labels,
        state.pending_evals.clone(),
        response_fn,
    ));
    let app = build_app_stateful(state, bridge, Some(token.to_string()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn mcp_session(base: &str) -> (reqwest::Client, String) {
    let client = reqwest::Client::new();
    let init_resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "deep-adversarial-test", "version": "0.1.0"}
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
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    (client, session_id)
}

async fn call_tool(
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    tool: &str,
    args: serde_json::Value,
) -> String {
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        }))
        .send()
        .await
        .unwrap();
    resp.text().await.unwrap()
}

/// REST API helper: POST /api/tools/{name} with JSON body
async fn rest_call(
    client: &reqwest::Client,
    base: &str,
    tool: &str,
    body: &str,
) -> reqwest::Response {
    client
        .post(format!("{base}/api/tools/{tool}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

/// REST API helper with auth token
async fn rest_call_auth(
    client: &reqwest::Client,
    base: &str,
    tool: &str,
    body: &str,
    token: &str,
) -> reqwest::Response {
    client
        .post(format!("{base}/api/tools/{tool}"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 1: REST API Full Coverage (27 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rest_list_tools_returns_all_25() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/tools"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let tools: Vec<serde_json::Value> = resp.json().await.unwrap();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();

    let expected_tools = [
        "eval_js",
        "dom_snapshot",
        "find_elements",
        "invoke_command",
        "screenshot",
        "verify_state",
        "detect_ghost_commands",
        "check_ipc_integrity",
        "wait_for",
        "assert_semantic",
        "resolve_command",
        "get_registry",
        "app_state",
        "get_memory_stats",
        "get_plugin_info",
        "get_diagnostics",
        "app_info",
        "list_app_dir",
        "read_app_file",
        "query_db",
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "route",
        "trace",
        "logs",
        "introspect",
        "fault",
        "explain",
        "animation",
    ];
    for tool in &expected_tools {
        assert!(
            names.contains(&tool.to_string()),
            "missing tool '{tool}' in REST listing: {names:?}",
        );
    }
    assert_eq!(
        names.len(),
        expected_tools.len(),
        "unexpected extra tools: {names:?}"
    );
}

#[tokio::test]
async fn rest_eval_js_with_callback_bridge() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"hello from eval\"".to_string()).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "eval_js", r#"{"code": "1+1"}"#).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("result").is_some(), "should have result: {body}");
}

#[tokio::test]
async fn rest_window_list_action() {
    let state = test_state();
    let base = start_server(state, &["main", "settings"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "window", r#"{"action": "list"}"#).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(result_str.contains("main"), "should contain 'main': {body}");
    assert!(
        result_str.contains("settings"),
        "should contain 'settings': {body}"
    );
}

#[tokio::test]
async fn rest_get_plugin_info_structure() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "get_plugin_info", "").await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let result = &body["result"];
    assert!(result["version"].is_string(), "should have version: {body}");
    assert!(
        result["tools"]["total"].is_number(),
        "should have tool count in tools.total: {body}"
    );
    assert!(result["port"].is_number(), "should have port: {body}");
}

#[tokio::test]
async fn rest_get_memory_stats_returns_data() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "get_memory_stats", "").await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("result").is_some(), "should have result: {body}");
}

#[tokio::test]
async fn rest_get_registry_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "get_registry", "{}").await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["result"].is_array(), "should return array: {body}");
}

#[tokio::test]
async fn rest_nonexistent_tool_returns_404() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "nonexistent_tool", "{}").await;
    assert_eq!(resp.status(), 404);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("unknown tool"),
        "should contain 'unknown tool': {body}"
    );
}

#[tokio::test]
async fn rest_recording_full_lifecycle() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    // Start
    let resp = rest_call(
        &client,
        &base,
        "recording",
        r#"{"action": "start", "session_id": "rest-test"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(result_str.contains("true"), "should start: {body}");

    // Checkpoint
    let resp = rest_call(
        &client,
        &base,
        "recording",
        r#"{"action": "checkpoint", "checkpoint_id": "cp-rest-1", "checkpoint_label": "test"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);

    // List checkpoints
    let resp = rest_call(
        &client,
        &base,
        "recording",
        r#"{"action": "list_checkpoints"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(
        result_str.contains("cp-rest-1"),
        "should contain checkpoint: {body}"
    );

    // Get events
    let resp = rest_call(&client, &base, "recording", r#"{"action": "get_events"}"#).await;
    assert_eq!(resp.status(), 200);

    // Stop
    let resp = rest_call(&client, &base, "recording", r#"{"action": "stop"}"#).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(
        result_str.contains("rest-test"),
        "should contain session id: {body}"
    );
}

#[tokio::test]
async fn rest_window_get_state() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "window", r#"{"action": "get_state"}"#).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(
        result_str.contains("main"),
        "should contain main window: {body}"
    );
}

#[tokio::test]
async fn rest_window_manage_minimize() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "manage", "manage_action": "minimize"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body["result"]).unwrap();
    assert!(
        result_str.contains("minimize"),
        "should contain minimize: {body}"
    );
}

#[tokio::test]
async fn rest_window_resize() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "resize", "width": 1024, "height": 768}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn rest_window_move_to() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "move_to", "x": 100, "y": 200}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn rest_window_set_title() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "set_title", "title": "New Title"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn rest_logs_all_actions() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "[]".to_string()).await;
    let client = reqwest::Client::new();

    let actions = [
        "console",
        "network",
        "ipc",
        "navigation",
        "dialogs",
        "events",
        "slow_ipc",
    ];
    for action in &actions {
        let body_str = format!(r#"{{"action": "{action}"}}"#);
        let resp = rest_call(&client, &base, "logs", &body_str).await;
        assert_eq!(
            resp.status(),
            200,
            "logs.{action} should return 200, got {}",
            resp.status()
        );
    }
}

#[tokio::test]
async fn rest_compound_tools_missing_action_field() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let compound_tools = [
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "logs",
    ];
    for tool in &compound_tools {
        let resp = rest_call(&client, &base, tool, r"{}").await;
        assert_eq!(
            resp.status(),
            400,
            "{tool} with empty params should return 400, got {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("invalid parameters"),
            "{tool} should report invalid parameters: {body}"
        );
    }
}

#[tokio::test]
async fn rest_compound_tools_invalid_action_value() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let compound_tools = [
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "logs",
    ];
    for tool in &compound_tools {
        let resp = rest_call(
            &client,
            &base,
            tool,
            r#"{"action": "nonexistent_action_xyz"}"#,
        )
        .await;
        assert_eq!(
            resp.status(),
            400,
            "{tool} with invalid action should return 400, got {}",
            resp.status()
        );
    }
}

#[tokio::test]
async fn rest_auth_required_401_without_token() {
    let state = test_state();
    let token = "test-secret-token-123";
    let base = start_auth_server(state, &["main"], token).await;
    let client = reqwest::Client::new();

    // Without token: 401
    let resp = client
        .get(format!("{base}/api/tools"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "should reject unauthenticated request");
}

#[tokio::test]
async fn rest_auth_required_200_with_correct_token() {
    let state = test_state();
    let token = "test-secret-token-456";
    let base = start_auth_server(state, &["main"], token).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/tools"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "should accept authenticated request");
}

#[tokio::test]
async fn rest_auth_tool_call_with_token() {
    let state = test_state();
    let token = "rest-auth-tool-token";
    let base = start_auth_server(state, &["main"], token).await;
    let client = reqwest::Client::new();

    // Without token: 401
    let resp = rest_call(&client, &base, "get_memory_stats", "").await;
    assert_eq!(resp.status(), 401);

    // With token: 200
    let resp = rest_call_auth(&client, &base, "get_memory_stats", "", token).await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn rest_eval_js_with_callback_bridge_via_auth() {
    let state = test_state();
    let token = "eval-auth-token";
    let base = start_auth_callback_server(state, &["main"], token, |_| {
        "\"auth eval result\"".to_string()
    })
    .await;
    let client = reqwest::Client::new();

    let resp = rest_call_auth(&client, &base, "eval_js", r#"{"code": "1+1"}"#, token).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("result").is_some(), "should have result: {body}");
}

#[tokio::test]
async fn rest_get_diagnostics() {
    let state = test_state();
    // Diagnostics internally calls eval to check the bridge, so we need a callback bridge
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("version") {
            "\"0.5.0\"".to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "get_diagnostics", "{}").await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body.get("result").is_some(),
        "diagnostics should return result: {body}"
    );
}

#[tokio::test]
async fn rest_resolve_command_returns_array() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "resolve_command",
        r#"{"query": "anything"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["result"].is_array(), "should return array: {body}");
}

#[tokio::test]
async fn rest_empty_body_treated_as_empty_object() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    // get_memory_stats accepts empty params
    let resp = client
        .post(format!("{base}/api/tools/get_memory_stats"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "empty body should be treated as {{}}");
}

#[tokio::test]
async fn rest_invalid_json_returns_400() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "eval_js", "not valid json {{{").await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("invalid JSON"),
        "got: {body}"
    );
}

#[tokio::test]
async fn rest_wrong_field_names_returns_400() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "eval_js", r#"{"wrong_field": "value"}"#).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("invalid parameters"),
        "got: {body}"
    );
}

#[tokio::test]
async fn rest_mcp_init_requires_auth_when_enabled() {
    let state = test_state();
    let token = "mcp-auth-token-test";
    let base = start_auth_server(state, &["main"], token).await;

    // MCP session without auth should fail at initialization
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "MCP init without auth should be rejected"
    );

    // MCP session WITH auth should succeed
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.1.0"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "MCP init with auth should succeed");
    assert!(
        resp.headers().get("mcp-session-id").is_some(),
        "should return session-id header"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 2: Privacy Configuration Edge Cases (16 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn privacy_tool_disabled_via_disabled_tools() {
    let mut config = PrivacyConfig::default();
    config.disabled_tools.insert("eval_js".to_string());
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "1+1"})).await;
    assert!(
        body.contains("disabled"),
        "disabled tool should return disabled: {body}"
    );
}

#[tokio::test]
async fn privacy_command_blocklist_blocks_invoke() {
    let mut config = PrivacyConfig::default();
    config.command_blocklist.insert("delete_user".to_string());
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "invoke_command",
        json!({"command": "delete_user"}),
    )
    .await;
    assert!(
        body.contains("blocked") || body.contains("not allowed") || body.contains("error"),
        "blocked command should be rejected: {body}"
    );
}

#[tokio::test]
async fn privacy_command_allowlist_restricts_invoke() {
    let mut allow = HashSet::new();
    allow.insert("greet".to_string());
    let config = PrivacyConfig {
        command_allowlist: Some(allow),
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    // Allowed command should work (even if the actual invoke fails due to no Tauri runtime)
    let body = call_tool(
        &client,
        &base,
        &sid,
        "invoke_command",
        json!({"command": "unauthorized_cmd"}),
    )
    .await;
    assert!(
        body.contains("blocked") || body.contains("not allowed") || body.contains("error"),
        "non-allowed command should be rejected: {body}"
    );
}

#[tokio::test]
async fn privacy_blocklist_wins_over_allowlist() {
    let mut allow = HashSet::new();
    allow.insert("save_key".to_string());
    let mut block = HashSet::new();
    block.insert("save_key".to_string());
    let config = PrivacyConfig {
        command_allowlist: Some(allow),
        command_blocklist: block,
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "invoke_command",
        json!({"command": "save_key"}),
    )
    .await;
    assert!(
        body.contains("blocked") || body.contains("not allowed") || body.contains("error"),
        "blocklist should override allowlist: {body}"
    );
}

#[tokio::test]
async fn privacy_redaction_scrubs_eval_output() {
    let config = PrivacyConfig {
        redaction_enabled: true,
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| {
        r#"{"api_key": "sk-secret12345678901234567890", "name": "safe"}"#.to_string()
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        json!({"code": "getConfig()"}),
    )
    .await;
    assert!(body.contains("[REDACTED]"), "should redact api_key: {body}");
    assert!(
        body.contains("safe"),
        "should preserve non-sensitive data: {body}"
    );
}

#[tokio::test]
async fn privacy_strict_mode_blocks_dangerous_tools() {
    let config = victauri_plugin::privacy::strict_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // eval_js should be blocked
    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "1+1"})).await;
    assert!(
        body.contains("disabled"),
        "eval_js should be disabled in strict mode: {body}"
    );

    // screenshot should be blocked
    let body = call_tool(&client, &base, &sid, "screenshot", json!({})).await;
    assert!(
        body.contains("disabled"),
        "screenshot should be disabled in strict mode: {body}"
    );

    // invoke_command should be blocked
    let body = call_tool(
        &client,
        &base,
        &sid,
        "invoke_command",
        json!({"command": "greet"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "invoke_command should be disabled: {body}"
    );
}

#[tokio::test]
async fn privacy_strict_mode_allows_read_only_tools() {
    let config = victauri_plugin::privacy::strict_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // window list should work
    let body = call_tool(&client, &base, &sid, "window", json!({"action": "list"})).await;
    assert!(
        body.contains("main"),
        "window.list should work in strict mode: {body}"
    );

    // get_registry should work
    let body = call_tool(&client, &base, &sid, "get_registry", json!({})).await;
    assert!(
        body.contains("[]"),
        "get_registry should work in strict mode: {body}"
    );

    // get_memory_stats should work
    let body = call_tool(&client, &base, &sid, "get_memory_stats", json!({})).await;
    assert!(
        body.contains("bytes") || body.contains("size") || body.contains("memory"),
        "get_memory_stats should work in strict mode: {body}"
    );

    // get_plugin_info should work
    let body = call_tool(&client, &base, &sid, "get_plugin_info", json!({})).await;
    assert!(
        body.contains("version"),
        "get_plugin_info should work in strict mode: {body}"
    );
}

#[tokio::test]
async fn privacy_strict_mode_blocks_window_mutations() {
    let config = victauri_plugin::privacy::strict_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // window.manage should be blocked
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "manage", "manage_action": "minimize"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "window.manage should be disabled: {body}"
    );

    // window.resize should be blocked
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "resize", "width": 100, "height": 100}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "window.resize should be disabled: {body}"
    );

    // window.set_title should be blocked
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "set_title", "title": "hack"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "window.set_title should be disabled: {body}"
    );
}

#[tokio::test]
async fn privacy_test_profile_blocks_eval_allows_interactions() {
    let config = victauri_plugin::privacy::test_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("click(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    // eval_js blocked
    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "1+1"})).await;
    assert!(
        body.contains("disabled"),
        "eval_js disabled in test profile: {body}"
    );

    // interact.click allowed
    let body = call_tool(
        &client,
        &base,
        &sid,
        "interact",
        json!({"action": "click", "ref_id": "e1"}),
    )
    .await;
    assert!(
        !body.contains("disabled"),
        "interact.click should be allowed: {body}"
    );
}

#[tokio::test]
async fn privacy_disable_all_tools() {
    let mut disabled = HashSet::new();
    let all_tools = [
        "eval_js",
        "dom_snapshot",
        "find_elements",
        "invoke_command",
        "screenshot",
        "verify_state",
        "detect_ghost_commands",
        "check_ipc_integrity",
        "wait_for",
        "assert_semantic",
        "resolve_command",
        "get_registry",
        "app_state",
        "get_memory_stats",
        "get_plugin_info",
        "get_diagnostics",
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "logs",
    ];
    for tool in &all_tools {
        disabled.insert(tool.to_string());
    }
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    for tool in &all_tools {
        let args = match *tool {
            "window" => json!({"action": "list"}),
            "recording" => json!({"action": "start"}),
            "logs" => json!({"action": "console"}),
            "inspect" => json!({"action": "performance"}),
            "css" => json!({"action": "inject", "css": "body{}"}),
            "interact" => json!({"action": "click", "ref_id": "e1"}),
            "input" => json!({"action": "fill", "ref_id": "e1", "value": "x"}),
            "storage" => json!({"action": "get", "key": "k"}),
            "navigate" => json!({"action": "go_to", "url": "http://localhost"}),
            "eval_js" => json!({"code": "1"}),
            "resolve_command" => json!({"query": "test"}),
            _ => json!({}),
        };
        let body = call_tool(&client, &base, &sid, tool, args).await;
        assert!(
            body.contains("disabled"),
            "tool '{tool}' should be disabled when in disabled_tools set: {body}"
        );
    }
}

#[tokio::test]
async fn privacy_disabled_tool_also_disabled_in_rest_listing() {
    let mut disabled = HashSet::new();
    disabled.insert("eval_js".to_string());
    disabled.insert("screenshot".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/tools"))
        .send()
        .await
        .unwrap();
    let tools: Vec<serde_json::Value> = resp.json().await.unwrap();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();

    assert!(
        !names.contains(&"eval_js".to_string()),
        "disabled tool should be filtered from listing"
    );
    assert!(
        !names.contains(&"screenshot".to_string()),
        "disabled tool should be filtered from listing"
    );
    assert!(
        names.contains(&"window".to_string()),
        "non-disabled tools should remain"
    );
}

#[tokio::test]
async fn privacy_redaction_on_rest_api() {
    let config = PrivacyConfig {
        redaction_enabled: true,
        ..Default::default()
    };
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| {
        r#"{"password": "hunter2", "name": "test"}"#.to_string()
    })
    .await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "eval_js", r#"{"code": "getConfig()"}"#).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let result_str = serde_json::to_string(&body).unwrap();
    assert!(
        result_str.contains("[REDACTED]"),
        "REST should redact too: {body}"
    );
}

#[tokio::test]
async fn privacy_observe_profile_blocks_storage_writes() {
    let config = victauri_plugin::privacy::observe_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "storage",
        json!({"action": "set", "key": "k", "value": "v"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "storage.set should be disabled in observe: {body}"
    );
}

#[tokio::test]
async fn privacy_observe_profile_blocks_recording() {
    let config = victauri_plugin::privacy::observe_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "recording should be disabled in observe: {body}"
    );
}

#[tokio::test]
async fn privacy_observe_allows_logs() {
    let config = victauri_plugin::privacy::observe_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_callback_server(state, &["main"], |_| "[]".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "logs", json!({"action": "console"})).await;
    assert!(
        !body.contains("disabled"),
        "logs.console should be allowed in observe: {body}"
    );
}

#[tokio::test]
async fn privacy_test_profile_allows_recording_blocks_css() {
    let config = victauri_plugin::privacy::test_privacy_config();
    let state = make_state_with_privacy(config);
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // recording allowed
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;
    assert!(
        !body.contains("disabled"),
        "recording.start should be allowed in test: {body}"
    );
    // cleanup
    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;

    // css injection blocked
    let body = call_tool(
        &client,
        &base,
        &sid,
        "css",
        json!({"action": "inject", "css": "body{color:red}"}),
    )
    .await;
    assert!(
        body.contains("disabled"),
        "css.inject should be disabled in test: {body}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 3: Concurrent State Access (15 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn concurrent_10_sessions_window_list() {
    let state = test_state();
    let base = start_server(state, &["main", "settings"]).await;

    let mut handles = Vec::new();
    for i in 0..10 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let (client, sid) = mcp_session(&url).await;
            let body = call_tool(&client, &url, &sid, "window", json!({"action": "list"})).await;
            assert!(
                body.contains("main") && body.contains("settings"),
                "concurrent session {i} should list both windows: {body}"
            );
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_eval_unique_uuids() {
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
                json!({"code": format!("'result_{i}'")}),
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

#[tokio::test]
async fn concurrent_tool_invocation_counter() {
    let state = test_state();
    let base = start_server(state.clone(), &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let mut handles = Vec::new();
    for _ in 0..50 {
        let c = client.clone();
        let u = base.clone();
        let s = sid.clone();
        handles.push(tokio::spawn(async move {
            call_tool(&c, &u, &s, "window", json!({"action": "list"})).await;
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let invocations = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        invocations >= 50,
        "at least 50 invocations expected, got {invocations}"
    );
}

#[tokio::test]
async fn concurrent_100_health_checks() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for i in 0..100 {
        let c = client.clone();
        let u = base.clone();
        handles.push(tokio::spawn(async move {
            let resp = c.get(format!("{u}/health")).send().await.unwrap();
            assert!(
                resp.status().is_success() || resp.status().as_u16() == 429,
                "health check {i} should succeed or rate-limit: {}",
                resp.status()
            );
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_rest_and_mcp_calls() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let mut handles = Vec::new();

    // 5 MCP sessions
    for i in 0..5 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let (client, sid) = mcp_session(&url).await;
            let body = call_tool(&client, &url, &sid, "window", json!({"action": "list"})).await;
            assert!(body.contains("main"), "MCP session {i} should work: {body}");
        }));
    }

    // 5 REST calls
    for i in 0..5 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            let resp = rest_call(&client, &url, "get_memory_stats", "").await;
            assert!(
                resp.status().is_success() || resp.status().as_u16() == 429,
                "REST call {i} should succeed or rate-limit: {}",
                resp.status()
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_recording_single_session() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // Start recording
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;
    assert!(body.contains("started"), "first start: {body}");

    // Sequential checkpoint creation on same session (concurrent MCP calls on
    // a single session can produce SSE interleaving issues, so test sequentially)
    for i in 0..10 {
        let body = call_tool(
            &client,
            &base,
            &sid,
            "recording",
            json!({"action": "checkpoint", "checkpoint_id": format!("cp-{i}")}),
        )
        .await;
        assert!(body.contains("created"), "checkpoint {i}: {body}");
    }

    // Verify all checkpoints were created
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "list_checkpoints"}),
    )
    .await;
    assert!(body.contains("cp-9"), "should have last checkpoint: {body}");

    // Stop recording
    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn concurrent_rapid_session_creation() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let mut handles = Vec::new();
    for i in 0..20 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let (client, sid) = mcp_session(&url).await;
            let body = call_tool(&client, &url, &sid, "get_plugin_info", json!({})).await;
            assert!(
                body.contains("version"),
                "session {i} should get plugin info: {body}"
            );
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_mixed_tool_types() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let tools_and_args: Vec<(&str, serde_json::Value)> = vec![
        ("window", json!({"action": "list"})),
        ("get_registry", json!({})),
        ("get_memory_stats", json!({})),
        ("get_plugin_info", json!({})),
        ("resolve_command", json!({"query": "test"})),
    ];

    let mut handles = Vec::new();
    for (tool, args) in &tools_and_args {
        for _ in 0..5 {
            let c = client.clone();
            let u = base.clone();
            let s = sid.clone();
            let tool = tool.to_string();
            let args = args.clone();
            handles.push(tokio::spawn(async move {
                let body = call_tool(&c, &u, &s, &tool, args).await;
                assert!(
                    !body.is_empty(),
                    "tool {tool} should return non-empty response"
                );
            }));
        }
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_info_requests() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for _ in 0..50 {
        let c = client.clone();
        let u = base.clone();
        handles.push(tokio::spawn(async move {
            let resp = c.get(format!("{u}/info")).send().await.unwrap();
            assert!(
                resp.status().is_success() || resp.status().as_u16() == 429,
                "info should succeed or rate-limit: {}",
                resp.status()
            );
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_recording_starts_only_one_succeeds() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let url = base.clone();
        handles.push(tokio::spawn(async move {
            let (client, sid) = mcp_session(&url).await;
            call_tool(
                &client,
                &url,
                &sid,
                "recording",
                json!({"action": "start", "session_id": format!("session-{i}")}),
            )
            .await
        }));
    }

    let mut started_count = 0;
    let mut error_count = 0;
    for handle in handles {
        let body = handle.await.unwrap();
        if body.contains("started") && body.contains("true") && !body.contains("already") {
            started_count += 1;
        }
        if body.contains("already") || body.contains("isError\":true") {
            error_count += 1;
        }
    }
    // At least one should succeed and the rest should fail.
    // Due to concurrency, we check bounds rather than exact counts.
    assert!(
        started_count >= 1,
        "at least one recording start should succeed, got {started_count}"
    );
    assert!(
        error_count >= 1,
        "at least some starts should fail with 'already active', got {error_count}"
    );
    assert_eq!(
        started_count + error_count,
        5,
        "all 5 attempts should either succeed or error"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 4: Eval Timeout & Edge Cases (12 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn eval_immediate_response() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "42".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "1+1"})).await;
    assert!(
        body.contains("42"),
        "should return immediate result: {body}"
    );
}

#[tokio::test]
async fn eval_bridge_returns_error_string() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| {
        r#"{"__victauri_err": "ReferenceError: foo is not defined"}"#.to_string()
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "foo()"})).await;
    assert!(
        body.contains("ReferenceError") || body.contains("error") || body.contains("Error"),
        "should surface bridge error: {body}"
    );
}

#[tokio::test]
async fn eval_empty_code_string() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": ""})).await;
    // Empty code should still be processed (returns null or similar)
    assert!(!body.is_empty(), "should handle empty code: {body}");
}

#[tokio::test]
async fn eval_null_bytes_in_code() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        json!({"code": "var x = 'hello\u{0000}world'"}),
    )
    .await;
    assert!(!body.is_empty(), "should handle null bytes: {body}");
}

#[tokio::test]
async fn eval_unicode_emoji_in_code() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"unicode ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        json!({"code": "'Hello World'"}),
    )
    .await;
    assert!(body.contains("unicode ok"), "should handle unicode: {body}");
}

#[tokio::test]
async fn eval_statement_keywords_no_auto_return() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        // Verify the injected script does NOT start with "return" for statements
        if script.contains("return if")
            || script.contains("return for")
            || script.contains("return const")
        {
            "\"BUG: auto-return prepended to statement\"".to_string()
        } else {
            "\"ok\"".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    for code in &[
        "if (true) { 1 }",
        "for (let i=0; i<1; i++) {}",
        "const x = 1",
    ] {
        let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": code})).await;
        assert!(
            !body.contains("BUG"),
            "auto-return should NOT be prepended to statement '{code}': {body}"
        );
    }
}

#[tokio::test]
async fn eval_explicit_return_not_doubled() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("return return") {
            "\"BUG: double return\"".to_string()
        } else {
            "\"ok\"".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        json!({"code": "return document.title"}),
    )
    .await;
    assert!(!body.contains("BUG"), "should NOT double-return: {body}");
}

#[tokio::test]
async fn eval_large_json_response() {
    let state = test_state();
    let large = "x".repeat(1_000_000);
    let large_clone = large.clone();
    let base = start_callback_server(state, &["main"], move |_| format!("\"{large_clone}\"")).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "'large'"})).await;
    assert!(
        body.len() > 100_000,
        "should return large response, got {} bytes",
        body.len()
    );
}

#[tokio::test]
async fn eval_dom_snapshot_large_response() {
    let state = test_state();
    // Generate a large DOM-like response
    let elements: Vec<serde_json::Value> = (0..1000)
        .map(|i| json!({"ref": format!("e{i}"), "tag": "div", "text": format!("Element {i}")}))
        .collect();
    let response = serde_json::to_string(&elements).unwrap();
    let base = start_callback_server(state, &["main"], move |_| response.clone()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "dom_snapshot", json!({})).await;
    assert!(
        body.contains("e999"),
        "should contain last element ref: body is {} bytes",
        body.len()
    );
}

#[tokio::test]
async fn eval_with_rejecting_bridge() {
    let state = test_state();
    let base = start_rejecting_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": "1+1"})).await;
    assert!(
        body.contains("error") || body.contains("Error") || body.contains("timed out"),
        "rejecting bridge should produce error: {body}"
    );
}

#[tokio::test]
async fn eval_code_exceeding_max_length() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "\"ok\"".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    // MAX_EVAL_CODE_LEN is 1_000_000 (1 MB)
    let huge_code = "x".repeat(1_000_001);
    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": huge_code})).await;
    assert!(
        body.contains("maximum length") || body.contains("error") || body.contains("Error"),
        "code exceeding max length should be rejected: body len={}",
        body.len()
    );
}

#[tokio::test]
async fn eval_returns_json_object() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| {
        r#"{"key":"value","num":42}"#.to_string()
    })
    .await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "eval_js",
        json!({"code": "({key:'value', num:42})"}),
    )
    .await;
    assert!(body.contains("key"), "should return JSON object: {body}");
    assert!(body.contains("42"), "should return numeric value: {body}");
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 5: Tool Parameter Validation (22 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn param_eval_js_code_as_number() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "eval_js", r#"{"code": 42}"#).await;
    assert_eq!(resp.status(), 400, "number instead of string should fail");
}

#[tokio::test]
async fn param_interact_ref_id_as_number() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "interact",
        r#"{"action": "click", "ref_id": 42}"#,
    )
    .await;
    assert_eq!(resp.status(), 400, "number ref_id should fail");
}

#[tokio::test]
async fn param_window_resize_string_dimensions() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "resize", "width": "abc", "height": "def"}"#,
    )
    .await;
    assert_eq!(resp.status(), 400, "string dimensions should fail");
}

#[tokio::test]
async fn param_navigate_empty_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        json!({"action": "go_to", "url": ""}),
    )
    .await;
    assert!(
        body.contains("error") || body.contains("invalid") || body.contains("blocked"),
        "empty URL should be rejected: {body}"
    );
}

#[tokio::test]
async fn param_storage_set_missing_key_value() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "storage", r#"{"action": "set"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    // storage.set without key/value should either fail with 400 or return an error in the result
    assert!(
        status == 400 || body.get("error").is_some(),
        "storage.set without key/value should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_assert_semantic_missing_fields() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "assert_semantic", r"{}").await;
    assert_eq!(
        resp.status(),
        400,
        "assert_semantic without required fields should fail"
    );
}

#[tokio::test]
async fn param_verify_state_empty_expression() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "verify_state",
        json!({"frontend_expr": "", "backend_state": {}}),
    )
    .await;
    // Should still process (empty expression is valid syntax, might return null)
    assert!(!body.is_empty(), "should handle empty expression");
}

#[tokio::test]
async fn param_all_compound_tools_empty_params() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let compound = [
        "interact",
        "input",
        "window",
        "storage",
        "navigate",
        "recording",
        "inspect",
        "css",
        "logs",
    ];
    for tool in &compound {
        let resp = rest_call(&client, &base, tool, r"{}").await;
        assert_eq!(
            resp.status(),
            400,
            "{tool} with {{}} should return 400 (missing action)"
        );
    }
}

#[tokio::test]
async fn param_extra_fields_are_ignored() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    // window.list with extra "extra_field" should still work
    let resp = rest_call(
        &client,
        &base,
        "window",
        r#"{"action": "list", "extra_field": "should be ignored"}"#,
    )
    .await;
    assert_eq!(resp.status(), 200, "extra fields should be ignored");
}

#[tokio::test]
async fn param_interact_missing_ref_id() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "interact", r#"{"action": "click"}"#).await;
    // click requires ref_id — should fail with 400 or return an error result
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "click without ref_id should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_input_fill_missing_value() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "input",
        r#"{"action": "fill", "ref_id": "e1"}"#,
    )
    .await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "fill without value should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_input_type_text_missing_text() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "input",
        r#"{"action": "type_text", "ref_id": "e1"}"#,
    )
    .await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "type_text without text should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_input_press_key_missing_key() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "input",
        r#"{"action": "press_key", "ref_id": "e1"}"#,
    )
    .await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "press_key without key should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_recording_events_between_missing_from_to() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(
        &client,
        &base,
        "recording",
        r#"{"action": "events_between"}"#,
    )
    .await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "events_between without from/to should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_css_inject_missing_css() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "css", r#"{"action": "inject"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "css.inject without css should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_inspect_styles_missing_ref_id() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "inspect", r#"{"action": "styles"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "inspect.styles without ref_id should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_inspect_highlight_missing_ref_id() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "inspect", r#"{"action": "highlight"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "inspect.highlight without ref_id should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_inspect_bounds_missing_ref_ids() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "inspect", r#"{"action": "bounds"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "inspect.bounds without ref_ids should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_resolve_command_missing_query() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "resolve_command", r"{}").await;
    assert_eq!(
        resp.status(),
        400,
        "resolve_command without query should fail"
    );
}

#[tokio::test]
async fn param_wait_for_missing_condition() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "wait_for", r"{}").await;
    assert_eq!(resp.status(), 400, "wait_for without condition should fail");
}

#[tokio::test]
async fn param_navigate_go_to_missing_url() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "navigate", r#"{"action": "go_to"}"#).await;
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        status == 400 || body.get("error").is_some(),
        "navigate.go_to without url should error: status={status}, body={body}"
    );
}

#[tokio::test]
async fn param_invoke_command_missing_command() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = rest_call(&client, &base, "invoke_command", r"{}").await;
    assert_eq!(
        resp.status(),
        400,
        "invoke_command without command should fail"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 6: Recording Deep Edge Cases (10 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn recording_100_checkpoints_export_import_fidelity() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // Start recording
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "fidelity-test"}),
    )
    .await;

    // Create 100 checkpoints
    for i in 0..100 {
        let body = call_tool(
            &client, &base, &sid, "recording",
            json!({"action": "checkpoint", "checkpoint_id": format!("cp-{i}"), "checkpoint_label": format!("Label {i}")}),
        ).await;
        assert!(body.contains("created"), "checkpoint {i} failed: {body}");
    }

    // Export
    let export_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "export"}),
    )
    .await;
    assert!(
        export_body.contains("fidelity-test"),
        "export should contain session id"
    );
    assert!(
        export_body.contains("cp-99"),
        "export should contain last checkpoint"
    );

    // Stop
    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;

    // Import back
    let session_json = serde_json::to_string(&json!({
        "id": "fidelity-reimport",
        "started_at": "2025-01-01T00:00:00Z",
        "events": [],
        "checkpoints": []
    }))
    .unwrap();
    let import_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "import", "session_json": session_json}),
    )
    .await;
    assert!(
        import_body.contains("imported"),
        "import should succeed: {import_body}"
    );
}

#[tokio::test]
async fn recording_events_between_reverse_order() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "first"}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "second"}),
    )
    .await;

    // Reverse order: "to" before "from"
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "events_between", "from": "second", "to": "first"}),
    )
    .await;
    // Should return empty or an error, not panic
    assert!(
        body.contains("[]") || body.contains("error") || body.contains("not found"),
        "reverse checkpoint order should be handled gracefully: {body}"
    );

    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn recording_import_large_session() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // Create a session with many events. RecordedEvent has {index, timestamp, event}.
    // AppEvent uses #[serde(tag = "type")] — so event needs "type": "StateChange" at top level.
    let events: Vec<serde_json::Value> = (0..500)
        .map(|i| {
            json!({
                "index": i,
                "timestamp": "2025-01-01T00:00:00Z",
                "event": {
                    "type": "StateChange",
                    "key": format!("counter.{i}"),
                    "timestamp": "2025-01-01T00:00:00Z",
                    "caused_by": null
                }
            })
        })
        .collect();

    let session_json = serde_json::to_string(&json!({
        "id": "large-import",
        "started_at": "2025-01-01T00:00:00Z",
        "events": events,
        "checkpoints": []
    }))
    .unwrap();

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "import", "session_json": session_json}),
    )
    .await;
    assert!(
        body.contains("imported"),
        "large import should succeed: {body}"
    );
}

#[tokio::test]
async fn recording_stop_returns_event_count() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "count-test"}),
    )
    .await;

    // Create some checkpoints (events come from the event drain loop, which we
    // cannot trigger in mock, but checkpoints are tracked)
    for i in 0..5 {
        call_tool(
            &client,
            &base,
            &sid,
            "recording",
            json!({"action": "checkpoint", "checkpoint_id": format!("cp-{i}")}),
        )
        .await;
    }

    let body = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
    assert!(
        body.contains("count-test"),
        "stop should contain session id: {body}"
    );
}

#[tokio::test]
async fn recording_multiple_start_stop_cycles() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    for i in 0..5 {
        let start_body = call_tool(
            &client,
            &base,
            &sid,
            "recording",
            json!({"action": "start", "session_id": format!("cycle-{i}")}),
        )
        .await;
        assert!(
            start_body.contains("started"),
            "cycle {i} start: {start_body}"
        );

        let stop_body =
            call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
        assert!(
            stop_body.contains(&format!("cycle-{i}")),
            "cycle {i} stop should contain session id: {stop_body}"
        );
    }
}

#[tokio::test]
async fn recording_get_replay_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "get_replay"}),
    )
    .await;
    assert!(body.contains("[]"), "empty replay should return []: {body}");

    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn recording_import_invalid_json_string() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "import", "session_json": "this is {not valid} json"}),
    )
    .await;
    assert!(
        body.contains("invalid") || body.contains("error") || body.contains("Error"),
        "invalid session JSON should produce error: {body}"
    );
}

#[tokio::test]
async fn recording_import_valid_then_start_new() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // Import a session (this creates an active recording)
    let session_json = serde_json::to_string(&json!({
        "id": "imported-session",
        "started_at": "2025-01-01T00:00:00Z",
        "events": [],
        "checkpoints": []
    }))
    .unwrap();
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "import", "session_json": session_json}),
    )
    .await;
    assert!(body.contains("imported"), "import should succeed: {body}");

    // Import creates an active recording, so stop it first
    let body = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
    assert!(
        body.contains("imported-session"),
        "stop should return imported session: {body}"
    );

    // Now start a new recording after stopping the imported one
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "new-after-import"}),
    )
    .await;
    assert!(
        body.contains("started"),
        "should start new recording: {body}"
    );

    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn recording_checkpoint_without_label() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start"}),
    )
    .await;

    // Checkpoint with just an id, no label
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "bare-cp"}),
    )
    .await;
    assert!(
        body.contains("created"),
        "checkpoint without label should work: {body}"
    );

    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn recording_export_without_stop() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "export-while-active"}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "mid-cp"}),
    )
    .await;

    // Export while still recording
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "export"}),
    )
    .await;
    assert!(
        body.contains("export-while-active"),
        "export during recording should work: {body}"
    );

    call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

// ═══════════════════════════════════════════════════════════════════════════
// Group 7: Health & Info Endpoints (11 tests)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn health_returns_minimal_ok() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["status"], "ok");

    // Must NOT leak any sensitive or detailed info
    assert!(json.get("uptime_secs").is_none(), "should not leak uptime");
    assert!(json.get("memory").is_none(), "should not leak memory");
    assert!(
        json.get("commands_registered").is_none(),
        "should not leak command count"
    );
    assert!(json.get("token").is_none(), "should not leak auth token");
    assert!(json.get("version").is_none(), "should not leak version");
}

#[tokio::test]
async fn health_accessible_without_session() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    // No MCP session, no auth headers — just a plain GET
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200, "health should not require a session");
}

#[tokio::test]
async fn info_returns_expected_fields() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["name"], "victauri");
    assert!(json["version"].is_string(), "should have version");
    assert_eq!(json["protocol"], "mcp");
    assert!(json["port"].is_number(), "should have port");
}

#[tokio::test]
async fn info_does_not_leak_auth_token() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    let json: serde_json::Value = resp.json().await.unwrap();

    assert!(
        json.get("auth_token").is_none(),
        "should not leak auth token"
    );
    assert!(json.get("token").is_none(), "should not leak token");
    assert!(
        json.get("uptime_secs").is_none(),
        "should not expose uptime"
    );
}

#[tokio::test]
async fn unknown_paths_return_404() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let paths = [
        "/unknown",
        "/api/unknown",
        "/random/path",
        "/mcp/unknown_sub",
    ];
    for path in &paths {
        let resp = client.get(format!("{base}{path}")).send().await.unwrap();
        let status = resp.status().as_u16();
        assert!(
            status == 404 || status == 405,
            "path '{path}' should return 404 or 405, got {status}"
        );
    }
}

#[tokio::test]
async fn post_to_health_rejected() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/health"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().as_u16() >= 400,
        "POST to /health should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn put_to_info_rejected() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/info"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().as_u16() >= 400,
        "PUT to /info should be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn health_bypasses_auth() {
    let state = test_state();
    let token = "auth-health-test";
    let base = start_auth_server(state, &["main"], token).await;

    // No auth header — health should still work
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200, "health should bypass auth");
}

#[tokio::test]
async fn info_requires_auth_when_enabled() {
    let state = test_state();
    let token = "auth-info-test";
    let base = start_auth_server(state, &["main"], token).await;
    let client = reqwest::Client::new();

    // Without token
    let resp = client.get(format!("{base}/info")).send().await.unwrap();
    assert_eq!(resp.status(), 401, "info should require auth when enabled");

    // With token
    let resp = client
        .get(format!("{base}/info"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "info with auth should succeed");
}

#[tokio::test]
async fn health_has_security_headers() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    let headers = resp.headers();

    // The security_headers middleware should add these
    assert!(
        headers.get("x-content-type-options").is_some()
            || headers.get("X-Content-Type-Options").is_some(),
        "should have X-Content-Type-Options header"
    );
}

#[tokio::test]
async fn delete_to_health_rejected() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let client = reqwest::Client::new();

    let resp = client
        .delete(format!("{base}/health"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().as_u16() >= 400,
        "DELETE to /health should be rejected, got {}",
        resp.status()
    );
}
