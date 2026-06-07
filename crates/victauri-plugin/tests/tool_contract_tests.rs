mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Mutex;

use victauri_core::{CommandRegistry, EventLog, EventRecorder};
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;
use victauri_plugin::mcp::build_app_stateful;
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
        probes: victauri_plugin::introspection::AppStateProbes::default(),
    })
}

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
        screencast: std::sync::Arc::new(victauri_plugin::screencast::Screencast::default()),
        probes: victauri_plugin::introspection::AppStateProbes::default(),
    })
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
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_test_server(state: Arc<VictauriState>, labels: &[&str]) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(SimpleMockBridge::new(labels));
    let app = build_app_stateful(state, bridge, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_privacy_server(config: PrivacyConfig, labels: &[&str]) -> String {
    let state = privacy_state(config);
    start_test_server(state, labels).await
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
                "clientInfo": {"name": "contract-test", "version": "0.1.0"}
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

fn contains_error(body: &str) -> bool {
    body.contains("\"isError\":true") || body.contains("\"is_error\":true")
}

fn contains_text(body: &str, text: &str) -> bool {
    body.contains(text)
}

// ── Standalone Tool Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn eval_js_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("document.title") {
            "\"My App\"".to_string()
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
        "eval_js",
        serde_json::json!({"code": "document.title"}),
    )
    .await;
    assert!(
        contains_text(&body, "My App"),
        "eval_js should return result: {body}"
    );
}

#[tokio::test]
async fn dom_snapshot_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("snapshot(") {
            r#"{"role":"document","name":"page","children":[]}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(&client, &base, &sid, "dom_snapshot", serde_json::json!({})).await;
    assert!(
        contains_text(&body, "document"),
        "dom_snapshot should return tree: {body}"
    );
}

#[tokio::test]
async fn find_elements_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("findElements(") {
            r#"[{"ref":"e1","role":"button","name":"Submit"}]"#.to_string()
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
        "find_elements",
        serde_json::json!({"text": "Submit"}),
    )
    .await;
    assert!(
        contains_text(&body, "button"),
        "find_elements should return matches: {body}"
    );
}

#[tokio::test]
async fn invoke_command_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("__TAURI_INTERNALS__.invoke(") {
            r#"{"greeting":"Hello, World"}"#.to_string()
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
        "invoke_command",
        serde_json::json!({"command": "greet", "args": {"name": "World"}}),
    )
    .await;
    assert!(
        contains_text(&body, "Hello"),
        "invoke_command should return result: {body}"
    );
}

#[tokio::test]
async fn screenshot_returns_error_in_mock() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(&client, &base, &sid, "screenshot", serde_json::json!({})).await;
    assert!(
        contains_text(&body, "cannot get window handle") || contains_text(&body, "native handle"),
        "screenshot in mock should fail gracefully: {body}"
    );
}

#[tokio::test]
async fn verify_state_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("return (") {
            r#"{"title":"App"}"#.to_string()
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
        "verify_state",
        serde_json::json!({"frontend_expr": "({title:'App'})", "backend_state": {"title": "App"}}),
    )
    .await;
    assert!(
        contains_text(&body, "passed"),
        "verify_state should pass: {body}"
    );
}

#[tokio::test]
async fn detect_ghost_commands_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        // detect_ghost_commands projects command names in JS, so the bridge
        // returns a string array rather than full IPC entries.
        if script.contains("getIpcLog") {
            r#"["greet"]"#.to_string()
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
        "detect_ghost_commands",
        serde_json::json!({}),
    )
    .await;
    assert!(
        contains_text(&body, "ghost") || contains_text(&body, "greet"),
        "detect_ghost_commands should return report: {body}"
    );
}

#[tokio::test]
async fn check_ipc_integrity_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getIpcLog") {
            r#"{"healthy":true,"total_calls":1,"pending_count":0,"stale_count":0,"error_count":0,"stale_calls":[],"errored_calls":[]}"#.to_string()
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
        "check_ipc_integrity",
        serde_json::json!({}),
    )
    .await;
    assert!(
        contains_text(&body, "healthy"),
        "check_ipc_integrity should return health: {body}"
    );
}

#[tokio::test]
async fn wait_for_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("waitFor(") {
            r#"{"matched":true,"elapsed_ms":50}"#.to_string()
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
        "wait_for",
        serde_json::json!({"condition": "text", "value": "Hello", "timeout_ms": 5000}),
    )
    .await;
    assert!(
        contains_text(&body, "matched"),
        "wait_for should return match: {body}"
    );
}

#[tokio::test]
async fn assert_semantic_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("return (") {
            "\"hello\"".to_string()
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
        "assert_semantic",
        serde_json::json!({
            "expression": "document.title",
            "label": "title check",
            "condition": "equals",
            "expected": "hello"
        }),
    )
    .await;
    assert!(
        contains_text(&body, "passed"),
        "assert_semantic should pass: {body}"
    );
}

#[tokio::test]
async fn resolve_command_happy_path() {
    let state = test_state();
    state.registry.register(
        victauri_core::CommandInfo::new("save_settings").with_description("Save settings"),
    );
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "resolve_command",
        serde_json::json!({"query": "save settings"}),
    )
    .await;
    assert!(
        contains_text(&body, "save_settings"),
        "resolve_command should find match: {body}"
    );
}

#[tokio::test]
async fn get_registry_happy_path() {
    let state = test_state();
    state
        .registry
        .register(victauri_core::CommandInfo::new("greet").with_description("Greet"));
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(&client, &base, &sid, "get_registry", serde_json::json!({})).await;
    assert!(
        contains_text(&body, "greet"),
        "get_registry should list commands: {body}"
    );
}

#[tokio::test]
async fn get_memory_stats_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "get_memory_stats",
        serde_json::json!({}),
    )
    .await;
    assert!(
        contains_text(&body, "working_set")
            || contains_text(&body, "rss")
            || contains_text(&body, "bytes"),
        "get_memory_stats should return memory info: {body}"
    );
}

#[tokio::test]
async fn get_plugin_info_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "get_plugin_info",
        serde_json::json!({}),
    )
    .await;
    assert!(
        contains_text(&body, "version"),
        "get_plugin_info should return version: {body}"
    );
    assert!(
        contains_text(&body, "uptime_secs"),
        "get_plugin_info should return uptime: {body}"
    );
}

// ── Compound Tool: interact ────────────────────────────────────────���──────

#[tokio::test]
async fn interact_click_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("click(") {
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
        "interact",
        serde_json::json!({"action": "click", "ref_id": "e1"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "click: {body}");
}

#[tokio::test]
async fn interact_double_click_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("doubleClick(") {
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
        "interact",
        serde_json::json!({"action": "double_click", "ref_id": "e2"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "double_click: {body}");
}

#[tokio::test]
async fn interact_hover_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("hover(") {
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
        "interact",
        serde_json::json!({"action": "hover", "ref_id": "e3"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "hover: {body}");
}

#[tokio::test]
async fn interact_focus_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("focusElement(") {
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
        "interact",
        serde_json::json!({"action": "focus", "ref_id": "e4"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "focus: {body}");
}

#[tokio::test]
async fn interact_scroll_into_view_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("scrollTo(") {
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
        "interact",
        serde_json::json!({"action": "scroll_into_view", "ref_id": "e5"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "scroll_into_view: {body}");
}

#[tokio::test]
async fn interact_select_option_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("selectOption(") {
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
        "interact",
        serde_json::json!({"action": "select_option", "ref_id": "e6", "values": ["opt1"]}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "select_option: {body}");
}

#[tokio::test]
async fn interact_click_missing_ref_id() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "interact",
        serde_json::json!({"action": "click"}),
    )
    .await;
    assert!(
        contains_text(&body, "ref_id"),
        "missing ref_id should error: {body}"
    );
}

#[tokio::test]
async fn interact_select_option_missing_ref_id() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "interact",
        serde_json::json!({"action": "select_option", "values": ["a"]}),
    )
    .await;
    assert!(
        contains_text(&body, "ref_id"),
        "missing ref_id for select_option: {body}"
    );
}

// ── Compound Tool: input ──────────────────────────────────────────────────

#[tokio::test]
async fn input_fill_happy_path() {
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
        serde_json::json!({"action": "fill", "ref_id": "e1", "value": "hello"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "fill: {body}");
}

#[tokio::test]
async fn input_type_text_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("type(") {
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
        serde_json::json!({"action": "type_text", "ref_id": "e2", "text": "world"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "type_text: {body}");
}

#[tokio::test]
async fn input_press_key_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("pressKey(") {
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
        serde_json::json!({"action": "press_key", "key": "Enter"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "press_key: {body}");
}

#[tokio::test]
async fn input_fill_missing_ref_id() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "input",
        serde_json::json!({"action": "fill", "value": "x"}),
    )
    .await;
    assert!(contains_text(&body, "ref_id"), "missing ref_id: {body}");
}

#[tokio::test]
async fn input_fill_missing_value() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "input",
        serde_json::json!({"action": "fill", "ref_id": "e1"}),
    )
    .await;
    assert!(contains_text(&body, "value"), "missing value: {body}");
}

#[tokio::test]
async fn input_press_key_missing_key() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "input",
        serde_json::json!({"action": "press_key"}),
    )
    .await;
    assert!(contains_text(&body, "key"), "missing key: {body}");
}

// ── Compound Tool: window ─────────────────────────────────────────────────

#[tokio::test]
async fn window_get_state_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main", "settings"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "get_state"}),
    )
    .await;
    assert!(
        contains_text(&body, "main"),
        "get_state should list windows: {body}"
    );
}

#[tokio::test]
async fn window_list_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main", "settings"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "list"}),
    )
    .await;
    assert!(
        contains_text(&body, "main"),
        "list should include main: {body}"
    );
    assert!(
        contains_text(&body, "settings"),
        "list should include settings: {body}"
    );
}

#[tokio::test]
async fn window_manage_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "manage", "manage_action": "minimize"}),
    )
    .await;
    assert!(
        contains_text(&body, "minimize"),
        "manage should execute: {body}"
    );
}

#[tokio::test]
async fn window_resize_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "resize", "width": 1024, "height": 768}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "resize should succeed: {body}");
}

#[tokio::test]
async fn window_move_to_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "move_to", "x": 100, "y": 200}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "move_to should succeed: {body}");
}

#[tokio::test]
async fn window_set_title_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "set_title", "title": "New Title"}),
    )
    .await;
    assert!(
        contains_text(&body, "ok"),
        "set_title should succeed: {body}"
    );
}

#[tokio::test]
async fn window_manage_missing_action() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "manage"}),
    )
    .await;
    assert!(
        contains_text(&body, "manage_action"),
        "missing manage_action: {body}"
    );
}

#[tokio::test]
async fn window_resize_missing_width() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "resize", "height": 600}),
    )
    .await;
    assert!(contains_text(&body, "width"), "missing width: {body}");
}

#[tokio::test]
async fn window_move_to_missing_y() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "move_to", "x": 10}),
    )
    .await;
    assert!(contains_text(&body, "y"), "missing y: {body}");
}

// ── Compound Tool: storage ────────────────────────────────────────────────

#[tokio::test]
async fn storage_get_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getLocalStorage(") {
            "\"stored_value\"".to_string()
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
        "storage",
        serde_json::json!({"action": "get", "key": "mykey"}),
    )
    .await;
    assert!(contains_text(&body, "stored_value"), "storage get: {body}");
}

#[tokio::test]
async fn storage_set_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("setLocalStorage(") {
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
        "storage",
        serde_json::json!({"action": "set", "key": "mykey", "value": "myval"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "storage set: {body}");
}

#[tokio::test]
async fn storage_delete_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("deleteLocalStorage(") {
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
        "storage",
        serde_json::json!({"action": "delete", "key": "mykey"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "storage delete: {body}");
}

#[tokio::test]
async fn storage_get_cookies_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getCookies(") {
            r#"[{"name":"session","value":"abc"}]"#.to_string()
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
        "storage",
        serde_json::json!({"action": "get_cookies"}),
    )
    .await;
    assert!(contains_text(&body, "session"), "get_cookies: {body}");
}

#[tokio::test]
async fn storage_set_missing_key() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "storage",
        serde_json::json!({"action": "set", "value": "x"}),
    )
    .await;
    assert!(contains_text(&body, "key"), "missing key: {body}");
}

// ── Compound Tool: navigate ───────────────────────────────────────────────

#[tokio::test]
async fn navigate_go_to_happy_path() {
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
    assert!(contains_text(&body, "ok"), "go_to: {body}");
}

#[tokio::test]
async fn navigate_go_back_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("navigateBack(") {
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
        serde_json::json!({"action": "go_back"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "go_back: {body}");
}

#[tokio::test]
async fn navigate_get_history_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getNavigationLog(") {
            r#"[{"url":"http://localhost","timestamp":1234}]"#.to_string()
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
        serde_json::json!({"action": "get_history"}),
    )
    .await;
    assert!(contains_text(&body, "localhost"), "get_history: {body}");
}

#[tokio::test]
async fn navigate_set_dialog_response_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("setDialogAutoResponse(") {
            r#"{"ok":true}"#.to_string()
        } else {
            "null".to_string()
        }
    })
    .await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client, &base, &sid, "navigate",
        serde_json::json!({"action": "set_dialog_response", "dialog_type": "confirm", "dialog_action": "accept"}),
    ).await;
    assert!(contains_text(&body, "ok"), "set_dialog_response: {body}");
}

#[tokio::test]
async fn navigate_get_dialog_log_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getDialogLog(") {
            r"[]".to_string()
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
        serde_json::json!({"action": "get_dialog_log"}),
    )
    .await;
    assert!(
        !contains_error(&body),
        "get_dialog_log should not error: {body}"
    );
}

#[tokio::test]
async fn navigate_go_to_missing_url() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to"}),
    )
    .await;
    assert!(contains_text(&body, "url"), "missing url: {body}");
}

// ── Compound Tool: recording ──────────────────────────────────────────────

#[tokio::test]
async fn recording_start_stop_happy_path() {
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
    assert!(contains_text(&body, "started"), "start: {body}");

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
    assert!(!contains_error(&body), "stop should not error: {body}");
}

#[tokio::test]
async fn recording_checkpoint_happy_path() {
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
    let body = call_tool(
        &client, &base, &sid, "recording",
        serde_json::json!({"action": "checkpoint", "checkpoint_id": "cp1", "checkpoint_label": "first"}),
    ).await;
    assert!(contains_text(&body, "created"), "checkpoint: {body}");
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
}

#[tokio::test]
async fn recording_list_checkpoints_happy_path() {
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
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "checkpoint", "checkpoint_id": "cp1"}),
    )
    .await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "list_checkpoints"}),
    )
    .await;
    assert!(contains_text(&body, "cp1"), "list_checkpoints: {body}");
    call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "stop"}),
    )
    .await;
}

#[tokio::test]
async fn recording_get_events_happy_path() {
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
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "get_events"}),
    )
    .await;
    assert!(
        !contains_error(&body),
        "get_events should not error: {body}"
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

#[tokio::test]
async fn recording_get_replay_happy_path() {
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
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "get_replay"}),
    )
    .await;
    assert!(
        !contains_error(&body),
        "get_replay should not error: {body}"
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

#[tokio::test]
async fn recording_export_happy_path() {
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
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "export"}),
    )
    .await;
    assert!(
        contains_text(&body, "events") || contains_text(&body, "checkpoints"),
        "export: {body}"
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

#[tokio::test]
async fn recording_import_happy_path() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let session_json = serde_json::json!({
        "id": "imported-session",
        "started_at": "2025-01-01T00:00:00Z",
        "ended_at": "2025-01-01T00:01:00Z",
        "events": [],
        "checkpoints": []
    });
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "import", "session_json": session_json.to_string()}),
    )
    .await;
    assert!(contains_text(&body, "imported"), "import: {body}");
}

#[tokio::test]
async fn recording_checkpoint_missing_id() {
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
    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        serde_json::json!({"action": "checkpoint"}),
    )
    .await;
    assert!(
        contains_text(&body, "checkpoint_id"),
        "missing checkpoint_id: {body}"
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

// ── Compound Tool: inspect ────────────────────────────────────────────────

#[tokio::test]
async fn inspect_get_styles_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getStyles(") {
            r#"{"display":"flex","color":"red"}"#.to_string()
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
        serde_json::json!({"action": "get_styles", "ref_id": "e1"}),
    )
    .await;
    assert!(contains_text(&body, "flex"), "get_styles: {body}");
}

#[tokio::test]
async fn inspect_get_bounding_boxes_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getBoundingBoxes(") {
            r#"[{"ref":"e1","x":0,"y":0,"width":100,"height":50}]"#.to_string()
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
        serde_json::json!({"action": "get_bounding_boxes", "ref_ids": ["e1"]}),
    )
    .await;
    assert!(contains_text(&body, "width"), "get_bounding_boxes: {body}");
}

#[tokio::test]
async fn inspect_highlight_happy_path() {
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
        serde_json::json!({"action": "highlight", "ref_id": "e1", "color": "rgba(255,0,0,0.3)"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "highlight: {body}");
}

#[tokio::test]
async fn inspect_clear_highlights_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("clearHighlights(") {
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
        serde_json::json!({"action": "clear_highlights"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "clear_highlights: {body}");
}

#[tokio::test]
async fn inspect_audit_accessibility_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("auditAccessibility(") {
            r#"{"summary":{"violations":0,"passes":5},"violations":[]}"#.to_string()
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
        serde_json::json!({"action": "audit_accessibility"}),
    )
    .await;
    assert!(
        contains_text(&body, "violations"),
        "audit_accessibility: {body}"
    );
}

#[tokio::test]
async fn inspect_get_performance_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getPerformanceMetrics(") {
            r#"{"navigation":{"load_event_ms":250},"js_heap":{"used_mb":10}}"#.to_string()
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
        serde_json::json!({"action": "get_performance"}),
    )
    .await;
    assert!(
        contains_text(&body, "navigation"),
        "get_performance: {body}"
    );
}

#[tokio::test]
async fn inspect_get_styles_missing_ref_id() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "inspect",
        serde_json::json!({"action": "get_styles"}),
    )
    .await;
    assert!(contains_text(&body, "ref_id"), "missing ref_id: {body}");
}

#[tokio::test]
async fn inspect_get_bounding_boxes_missing_ref_ids() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "inspect",
        serde_json::json!({"action": "get_bounding_boxes"}),
    )
    .await;
    assert!(contains_text(&body, "ref_ids"), "missing ref_ids: {body}");
}

// ── Compound Tool: css ────────────────────────────────────────────────────

#[tokio::test]
async fn css_inject_happy_path() {
    let state = test_state();
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
        serde_json::json!({"action": "inject", "css": "body { color: red; }"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "css inject: {body}");
}

#[tokio::test]
async fn css_remove_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("removeInjectedCss(") {
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
        serde_json::json!({"action": "remove"}),
    )
    .await;
    assert!(contains_text(&body, "ok"), "css remove: {body}");
}

#[tokio::test]
async fn css_inject_missing_css() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "css",
        serde_json::json!({"action": "inject"}),
    )
    .await;
    assert!(contains_text(&body, "css"), "missing css: {body}");
}

// ── Compound Tool: logs ───────────────────────────────────────────────────

#[tokio::test]
async fn logs_console_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getConsoleLogs(") {
            r#"[{"level":"log","message":"hello","timestamp":1234}]"#.to_string()
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
        "logs",
        serde_json::json!({"action": "console"}),
    )
    .await;
    assert!(contains_text(&body, "hello"), "console: {body}");
}

#[tokio::test]
async fn logs_network_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getNetworkLog(") {
            r#"[{"url":"http://api.example.com","status":200}]"#.to_string()
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
        "logs",
        serde_json::json!({"action": "network"}),
    )
    .await;
    assert!(contains_text(&body, "api.example"), "network: {body}");
}

#[tokio::test]
async fn logs_ipc_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getIpcLog(") {
            r#"[{"command":"greet","status":200}]"#.to_string()
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
        "logs",
        serde_json::json!({"action": "ipc"}),
    )
    .await;
    assert!(contains_text(&body, "greet"), "ipc: {body}");
}

#[tokio::test]
async fn logs_navigation_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getNavigationLog(") {
            r"[]".to_string()
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
        "logs",
        serde_json::json!({"action": "navigation"}),
    )
    .await;
    assert!(!contains_error(&body), "navigation: {body}");
}

#[tokio::test]
async fn logs_dialogs_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getDialogLog(") {
            r"[]".to_string()
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
        "logs",
        serde_json::json!({"action": "dialogs"}),
    )
    .await;
    assert!(!contains_error(&body), "dialogs: {body}");
}

#[tokio::test]
async fn logs_events_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getEventStream(") {
            r"[]".to_string()
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
        "logs",
        serde_json::json!({"action": "events"}),
    )
    .await;
    assert!(!contains_error(&body), "events: {body}");
}

#[tokio::test]
async fn logs_slow_ipc_happy_path() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("getIpcLog()") {
            r#"[{"command":"slow_cmd","duration_ms":500},{"command":"fast_cmd","duration_ms":5}]"#
                .to_string()
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
        "logs",
        serde_json::json!({"action": "slow_ipc", "threshold_ms": 100}),
    )
    .await;
    assert!(contains_text(&body, "slow_cmd"), "slow_ipc: {body}");
}

#[tokio::test]
async fn logs_slow_ipc_missing_threshold() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "logs",
        serde_json::json!({"action": "slow_ipc"}),
    )
    .await;
    assert!(
        contains_text(&body, "threshold_ms"),
        "missing threshold_ms: {body}"
    );
}

// ── Privacy Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn privacy_eval_js_disabled() {
    let mut disabled = HashSet::new();
    disabled.insert("eval_js".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let base = start_privacy_server(config, &["main"]).await;
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
        contains_text(&body, "disabled"),
        "disabled tool should reject: {body}"
    );
}

#[tokio::test]
async fn privacy_screenshot_disabled() {
    let mut disabled = HashSet::new();
    disabled.insert("screenshot".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let base = start_privacy_server(config, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(&client, &base, &sid, "screenshot", serde_json::json!({})).await;
    assert!(
        contains_text(&body, "disabled"),
        "disabled screenshot: {body}"
    );
}

#[tokio::test]
async fn privacy_navigate_disabled() {
    let mut disabled = HashSet::new();
    disabled.insert("navigate".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let base = start_privacy_server(config, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "navigate",
        serde_json::json!({"action": "go_to", "url": "https://example.com"}),
    )
    .await;
    assert!(
        contains_text(&body, "disabled"),
        "disabled navigate: {body}"
    );
}

#[tokio::test]
async fn privacy_inject_css_disabled() {
    let mut disabled = HashSet::new();
    disabled.insert("inject_css".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "css",
        serde_json::json!({"action": "inject", "css": "body{}"}),
    )
    .await;
    assert!(
        contains_text(&body, "disabled"),
        "disabled inject_css: {body}"
    );
}

#[tokio::test]
async fn privacy_fill_disabled() {
    let mut disabled = HashSet::new();
    disabled.insert("fill".to_string());
    let config = PrivacyConfig {
        disabled_tools: disabled,
        ..Default::default()
    };
    let state = privacy_state(config);
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "input",
        serde_json::json!({"action": "fill", "ref_id": "e1", "value": "x"}),
    )
    .await;
    assert!(contains_text(&body, "disabled"), "disabled fill: {body}");
}

// ── Tool Invocation Counter ───────────────────────────────────────────────

#[tokio::test]
async fn tool_invocations_increment() {
    let state = test_state();
    let base = start_test_server(state.clone(), &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let before = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "list"}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "list"}),
    )
    .await;
    let after = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "tool_invocations should increment: before={before} after={after}"
    );
}

#[tokio::test]
async fn tool_invocations_increment_for_compound_tools() {
    let state = test_state();
    let base = start_test_server(state.clone(), &["main", "settings"]).await;
    let (client, sid) = mcp_session(&base).await;

    let before = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "get_state"}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "resize", "width": 800, "height": 600}),
    )
    .await;
    call_tool(
        &client,
        &base,
        &sid,
        "get_memory_stats",
        serde_json::json!({}),
    )
    .await;
    let after = state
        .tool_invocations
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after >= before + 3,
        "counter should increment for each call: before={before} after={after}"
    );
}

// ── Edge cases ────────────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_tool_returns_error() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "nonexistent_tool",
        serde_json::json!({}),
    )
    .await;
    assert!(
        contains_text(&body, "error")
            || contains_text(&body, "not found")
            || contains_text(&body, "unknown"),
        "unknown tool should error: {body}"
    );
}

#[tokio::test]
async fn invalid_action_in_compound_tool() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        serde_json::json!({"action": "fly_away"}),
    )
    .await;
    assert!(
        contains_text(&body, "error") || contains_text(&body, "unknown"),
        "invalid action should error: {body}"
    );
}

#[tokio::test]
async fn navigate_go_to_blocks_javascript_url() {
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
        contains_text(&body, "error")
            || contains_text(&body, "invalid")
            || contains_text(&body, "blocked")
            || contains_text(&body, "javascript"),
        "javascript: URL should be blocked: {body}"
    );
}

// ── P1: wait_for async-completion (expression + event) ───────────────────────

/// POST args to the sessionless REST endpoint and return the response body.
async fn rest_call(base: &str, tool: &str, args: serde_json::Value) -> String {
    reqwest::Client::new()
        .post(format!("{base}/api/tools/{tool}"))
        .json(&args)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn wait_for_expression_polls_until_truthy() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = test_state();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_probe = Arc::clone(&calls);
    let base = start_callback_server(state, &["main"], move |script| {
        if script.contains("__TEST_READY__") {
            // Not ready for the first two polls, then becomes ready.
            let n = calls_probe.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                "false".to_string()
            } else {
                "true".to_string()
            }
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
        "wait_for",
        serde_json::json!({
            "condition": "expression",
            "value": "window.__TEST_READY__",
            "timeout_ms": 5000,
            "poll_ms": 30
        }),
    )
    .await;
    assert!(
        !contains_error(&body),
        "wait_for expression errored: {body}"
    );
    assert!(
        !contains_text(&body, "timeout after"),
        "expression should have become truthy before timeout: {body}"
    );
    assert!(
        contains_text(&body, "elapsed_ms"),
        "result should report elapsed_ms: {body}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "should have polled until truthy (>=3 evals), got {}",
        calls.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn wait_for_expression_matches_expected_value() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |script| {
        if script.contains("__STATUS__") {
            "\"done\"".to_string()
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
        "wait_for",
        serde_json::json!({
            "condition": "expression",
            "value": "window.__STATUS__",
            "expected": "done",
            "timeout_ms": 2000,
            "poll_ms": 30
        }),
    )
    .await;
    assert!(
        !contains_error(&body),
        "wait_for expression errored: {body}"
    );
    assert!(
        !contains_text(&body, "timeout after"),
        "expression should equal expected before timeout: {body}"
    );
}

#[tokio::test]
async fn wait_for_expression_requires_value() {
    let state = test_state();
    let base = start_callback_server(state, &["main"], |_| "null".to_string()).await;
    let (client, sid) = mcp_session(&base).await;
    let body = call_tool(
        &client,
        &base,
        &sid,
        "wait_for",
        serde_json::json!({"condition": "expression", "timeout_ms": 500}),
    )
    .await;
    assert!(
        contains_error(&body) && contains_text(&body, "value"),
        "missing expression value should error: {body}"
    );
}

#[tokio::test]
async fn wait_for_event_matches_recent_event() {
    let state = test_state();
    // A valid, far-future timestamp is unconditionally within the look-back window.
    state
        .event_bus
        .push(victauri_plugin::introspection::CapturedTauriEvent {
            name: "analysis-complete".to_string(),
            payload: r#"{"scored":437}"#.to_string(),
            timestamp: "2099-01-01T00:00:00Z".to_string(),
        });
    let base = start_test_server(state, &["main"]).await;
    let body = rest_call(
        &base,
        "wait_for",
        serde_json::json!({
            "condition": "event",
            "value": "analysis-complete",
            "timeout_ms": 2000,
            "poll_ms": 30
        }),
    )
    .await;
    assert!(
        body.contains("analysis-complete") && body.contains("437"),
        "event wait should return the matched event + payload: {body}"
    );
}

#[tokio::test]
async fn wait_for_event_outside_lookback_times_out() {
    let state = test_state();
    // An old event predates the default 2s look-back, so it must NOT satisfy the wait.
    state
        .event_bus
        .push(victauri_plugin::introspection::CapturedTauriEvent {
            name: "analysis-complete".to_string(),
            payload: "{}".to_string(),
            timestamp: "2000-01-01T00:00:00Z".to_string(),
        });
    let base = start_test_server(state, &["main"]).await;
    let body = rest_call(
        &base,
        "wait_for",
        serde_json::json!({
            "condition": "event",
            "value": "analysis-complete",
            "timeout_ms": 300,
            "poll_ms": 30
        }),
    )
    .await;
    assert!(
        body.contains("timeout after") && body.contains("listen_events"),
        "stale event should time out with an actionable hint: {body}"
    );
}

// ── P2: app_state probes ─────────────────────────────────────────────────────

#[tokio::test]
async fn app_state_lists_and_runs_probes() {
    let state = test_state();
    state.probes.register(
        "scoring",
        std::sync::Arc::new(|| serde_json::json!({ "pipeline_version": 5, "stale_items": 0 })),
    );
    let base = start_test_server(state, &["main"]).await;

    let list = rest_call(&base, "app_state", serde_json::json!({})).await;
    assert!(
        list.contains("scoring"),
        "should list the probe name: {list}"
    );

    let run = rest_call(&base, "app_state", serde_json::json!({"probe": "scoring"})).await;
    assert!(
        run.contains("pipeline_version") && run.contains('5'),
        "should run the probe and return its snapshot: {run}"
    );

    let unknown = rest_call(&base, "app_state", serde_json::json!({"probe": "nope"})).await;
    assert!(
        unknown.contains("unknown probe"),
        "unknown probe should return an actionable error: {unknown}"
    );
}

#[tokio::test]
async fn app_state_lists_empty_when_no_probes() {
    let state = test_state();
    let base = start_test_server(state, &["main"]).await;
    let list = rest_call(&base, "app_state", serde_json::json!({})).await;
    assert!(
        list.contains("probes"),
        "app_state with no probes should still return a probes list: {list}"
    );
}
