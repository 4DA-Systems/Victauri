use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use victauri_test::{
    VictauriClient, assert_ipc_healthy, assert_json_eq, assert_json_truthy,
    assert_no_a11y_violations, assert_performance_budget, assert_state_matches,
};

// ── Mock MCP Server ───────────────────────────────────────────────────────

#[derive(Clone)]
struct MockState {
    call_log: Arc<Mutex<Vec<(String, Value)>>>,
    request_count: Arc<AtomicU64>,
    session_id: String,
    response_override: Arc<Mutex<Option<Value>>>,
}

impl MockState {
    fn new() -> Self {
        Self {
            call_log: Arc::new(Mutex::new(Vec::new())),
            request_count: Arc::new(AtomicU64::new(0)),
            session_id: uuid::Uuid::new_v4().to_string(),
            response_override: Arc::new(Mutex::new(None)),
        }
    }
}

async fn mock_mcp_handler(State(state): State<MockState>, body: String) -> Response {
    state.request_count.fetch_add(1, Ordering::Relaxed);

    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "invalid json").into_response();
        }
    };

    let method = parsed["method"].as_str().unwrap_or("");
    let id = parsed.get("id").cloned();

    match method {
        "initialize" => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "mock-victauri", "version": "0.1.0"}
                }
            });
            let mut response = axum::Json(resp).into_response();
            response
                .headers_mut()
                .insert("mcp-session-id", state.session_id.parse().unwrap());
            response
        }
        "notifications/initialized" => StatusCode::OK.into_response(),
        "tools/call" => {
            let tool_name = parsed["params"]["name"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let arguments = parsed["params"]["arguments"].clone();
            state
                .call_log
                .lock()
                .await
                .push((tool_name.clone(), arguments.clone()));

            if let Some(override_resp) = state.response_override.lock().await.as_ref() {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": override_resp.clone()
                });
                return axum::Json(resp).into_response();
            }

            let content_text = match tool_name.as_str() {
                "eval_js" => json!({"result": "evaluated"}).to_string(),
                "dom_snapshot" => json!({"role": "document", "children": []}).to_string(),
                "find_elements" => json!([{"ref": "e1", "role": "button"}]).to_string(),
                "interact" => json!({"ok": true}).to_string(),
                "input" => json!({"ok": true}).to_string(),
                "window" => {
                    let action = arguments["action"].as_str().unwrap_or("");
                    match action {
                        "list" => json!(["main", "settings"]).to_string(),
                        "get_state" => json!([{"label": "main", "visible": true}]).to_string(),
                        _ => json!({"ok": true}).to_string(),
                    }
                }
                "screenshot" => json!({"image": "iVBORw0KGgo..."}).to_string(),
                "invoke_command" => json!({"result": "invoked"}).to_string(),
                "logs" => json!([{"command": "greet"}]).to_string(),
                "verify_state" => json!({"passed": true, "divergences": []}).to_string(),
                "detect_ghost_commands" => {
                    json!({"ghost_commands": [], "uncalled_commands": []}).to_string()
                }
                "check_ipc_integrity" => {
                    json!({"healthy": true, "stale_calls": 0, "error_calls": 0}).to_string()
                }
                "wait_for" => json!({"matched": true}).to_string(),
                "assert_semantic" => json!({"passed": true, "actual": "hello"}).to_string(),
                "resolve_command" => json!([{"name": "save", "score": 0.9}]).to_string(),
                "get_registry" => json!([{"name": "greet"}]).to_string(),
                "get_memory_stats" => json!({"working_set_bytes": 50000000}).to_string(),
                "get_plugin_info" => json!({"version": "0.1.0", "uptime_secs": 100}).to_string(),
                "storage" => json!({"ok": true}).to_string(),
                "navigate" => json!({"ok": true}).to_string(),
                "recording" => json!({"started": true, "session_id": "test"}).to_string(),
                "inspect" => {
                    let action = arguments["action"].as_str().unwrap_or("");
                    match action {
                        "audit_accessibility" => json!({"summary": {"violations": 0, "passes": 5}}).to_string(),
                        "get_performance" => json!({"navigation": {"load_event_ms": 250.0}, "js_heap": {"used_mb": 10.0}}).to_string(),
                        _ => json!({"ok": true}).to_string(),
                    }
                }
                "css" => json!({"ok": true}).to_string(),
                _ => json!({"error": "unknown tool"}).to_string(),
            };

            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": content_text}]
                }
            });
            axum::Json(resp).into_response()
        }
        _ => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "method not found"}
            });
            axum::Json(resp).into_response()
        }
    }
}

async fn start_mock_server(state: MockState) -> u16 {
    let app = Router::new()
        .route("/mcp", post(mock_mcp_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

// ── Connection Lifecycle Tests ────────────────────────────────────────────

#[tokio::test]
async fn connect_to_mock_server() {
    let state = MockState::new();
    let port = start_mock_server(state).await;
    let client = VictauriClient::connect(port).await;
    assert!(client.is_ok(), "should connect: {:?}", client.err());
}

#[tokio::test]
async fn connect_server_not_running() {
    let result = VictauriClient::connect(19999).await;
    assert!(result.is_err(), "should fail when server is not running");
}

#[tokio::test]
async fn session_id_is_set() {
    let state = MockState::new();
    let expected_sid = state.session_id.clone();
    let port = start_mock_server(state).await;
    let client = VictauriClient::connect(port).await.unwrap();
    assert_eq!(client.session_id(), expected_sid);
}

#[tokio::test]
async fn base_url_is_correct() {
    let state = MockState::new();
    let port = start_mock_server(state).await;
    let client = VictauriClient::connect(port).await.unwrap();
    assert!(client.base_url().contains(&port.to_string()));
}

// ── eval_js ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_eval_js_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.eval_js("document.title").await;

    let log = state.call_log.lock().await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "eval_js");
    assert_eq!(log[0].1["code"], "document.title");
}

// ── dom_snapshot ──────────────────────────────────────────────────────────

#[tokio::test]
async fn client_dom_snapshot_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.dom_snapshot().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "dom_snapshot");
}

// ── click ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_click_sends_interact() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.click("e5").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "interact");
    assert_eq!(log[0].1["action"], "click");
    assert_eq!(log[0].1["ref_id"], "e5");
}

// ── fill ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_fill_sends_input() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.fill("e2", "hello world").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "input");
    assert_eq!(log[0].1["action"], "fill");
    assert_eq!(log[0].1["ref_id"], "e2");
    assert_eq!(log[0].1["value"], "hello world");
}

// ── type_text ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_type_text_sends_input() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.type_text("e3", "typing").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "input");
    assert_eq!(log[0].1["action"], "type_text");
    assert_eq!(log[0].1["ref_id"], "e3");
    assert_eq!(log[0].1["text"], "typing");
}

// ── press_key ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_press_key_sends_input() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.press_key("Enter").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "input");
    assert_eq!(log[0].1["action"], "press_key");
    assert_eq!(log[0].1["key"], "Enter");
}

// ── hover ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_hover_sends_interact() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.hover("e7").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "interact");
    assert_eq!(log[0].1["action"], "hover");
    assert_eq!(log[0].1["ref_id"], "e7");
}

// ── focus ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_focus_sends_interact() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.focus("e8").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "interact");
    assert_eq!(log[0].1["action"], "focus");
    assert_eq!(log[0].1["ref_id"], "e8");
}

// ── scroll_to ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_scroll_to_sends_interact() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.scroll_to("e9").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "interact");
    assert_eq!(log[0].1["action"], "scroll_into_view");
    assert_eq!(log[0].1["ref_id"], "e9");
}

// ── select_option ─────────────────────────────────────────────────────────

#[tokio::test]
async fn client_select_option_sends_interact() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.select_option("e10", &["opt1", "opt2"]).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "interact");
    assert_eq!(log[0].1["action"], "select_option");
    assert_eq!(log[0].1["ref_id"], "e10");
    assert_eq!(log[0].1["values"], json!(["opt1", "opt2"]));
}

// ── list_windows ──────────────────────────────────────────────────────────

#[tokio::test]
async fn client_list_windows_sends_window() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.list_windows().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "window");
    assert_eq!(log[0].1["action"], "list");
}

// ── get_window_state ──────────────────────────────────────────────────────

#[tokio::test]
async fn client_get_window_state_sends_window() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_window_state(Some("main")).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "window");
    assert_eq!(log[0].1["action"], "get_state");
    assert_eq!(log[0].1["label"], "main");
}

#[tokio::test]
async fn client_get_window_state_no_label() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_window_state(None).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "window");
    assert_eq!(log[0].1["action"], "get_state");
    assert!(log[0].1.get("label").is_none() || log[0].1["label"].is_null());
}

// ── screenshot ────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_screenshot_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.screenshot().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "screenshot");
}

// ── invoke_command ────────────────────────────────────────────────────────

#[tokio::test]
async fn client_invoke_command_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client
        .invoke_command("greet", Some(json!({"name": "World"})))
        .await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "invoke_command");
    assert_eq!(log[0].1["command"], "greet");
    assert_eq!(log[0].1["args"]["name"], "World");
}

#[tokio::test]
async fn client_invoke_command_no_args() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.invoke_command("get_status", None).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "invoke_command");
    assert_eq!(log[0].1["command"], "get_status");
}

// ── get_ipc_log ──────────────────────────────────────────────────────────

#[tokio::test]
async fn client_get_ipc_log_sends_logs() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_ipc_log(Some(50)).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "logs");
    assert_eq!(log[0].1["action"], "ipc");
    assert_eq!(log[0].1["limit"], 50);
}

// ── verify_state ──────────────────────────────────────────────────────────

#[tokio::test]
async fn client_verify_state_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client
        .verify_state("({title:'App'})", json!({"title": "App"}))
        .await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "verify_state");
    assert_eq!(log[0].1["frontend_expr"], "({title:'App'})");
    assert_eq!(log[0].1["backend_state"]["title"], "App");
}

// ── detect_ghost_commands ─────────────────────────────────────────────────

#[tokio::test]
async fn client_detect_ghost_commands_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.detect_ghost_commands().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "detect_ghost_commands");
}

// ── check_ipc_integrity ───────────────────────────────────────────────────

#[tokio::test]
async fn client_check_ipc_integrity_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.check_ipc_integrity().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "check_ipc_integrity");
}

// ── assert_semantic ───────────────────────────────────────────────────────

#[tokio::test]
async fn client_assert_semantic_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client
        .assert_semantic("document.title", "title check", "equals", json!("App"))
        .await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "assert_semantic");
    assert_eq!(log[0].1["expression"], "document.title");
    assert_eq!(log[0].1["label"], "title check");
    assert_eq!(log[0].1["condition"], "equals");
    assert_eq!(log[0].1["expected"], "App");
}

// ── audit_accessibility ───────────────────────────────────────────────────

#[tokio::test]
async fn client_audit_accessibility_sends_inspect() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.audit_accessibility().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "inspect");
    assert_eq!(log[0].1["action"], "audit_accessibility");
}

// ── get_performance_metrics ───────────────────────────────────────────────

#[tokio::test]
async fn client_get_performance_metrics_sends_inspect() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_performance_metrics().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "inspect");
    assert_eq!(log[0].1["action"], "get_performance");
}

// ── get_registry ──────────────────────────────────────────────────────────

#[tokio::test]
async fn client_get_registry_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_registry().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "get_registry");
}

// ── get_memory_stats ──────────────────────────────────────────────────────

#[tokio::test]
async fn client_get_memory_stats_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_memory_stats().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "get_memory_stats");
}

// ── get_plugin_info ───────────────────────────────────────────────────────

#[tokio::test]
async fn client_get_plugin_info_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.get_plugin_info().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "get_plugin_info");
}

// ── wait_for ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_wait_for_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client
        .wait_for("text", Some("Hello"), Some(5000), Some(200))
        .await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "wait_for");
    assert_eq!(log[0].1["condition"], "text");
    assert_eq!(log[0].1["value"], "Hello");
    assert_eq!(log[0].1["timeout_ms"], 5000);
    assert_eq!(log[0].1["poll_ms"], 200);
}

#[tokio::test]
async fn client_wait_for_minimal_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.wait_for("ipc_idle", None, None, None).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "wait_for");
    assert_eq!(log[0].1["condition"], "ipc_idle");
}

// ── navigate ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_navigate_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.navigate("https://example.com").await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "navigate");
    assert_eq!(log[0].1["action"], "go_to");
    assert_eq!(log[0].1["url"], "https://example.com");
}

// ── logs ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_logs_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.logs("console", Some(10)).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "logs");
    assert_eq!(log[0].1["action"], "console");
    assert_eq!(log[0].1["limit"], 10);
}

// ── start_recording ──────────────────────────────────────────────────────

#[tokio::test]
async fn client_start_recording_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.start_recording(Some("my-session")).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "recording");
    assert_eq!(log[0].1["action"], "start");
    assert_eq!(log[0].1["session_id"], "my-session");
}

#[tokio::test]
async fn client_start_recording_no_id() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.start_recording(None).await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "recording");
    assert_eq!(log[0].1["action"], "start");
}

// ── stop_recording ───────────────────────────────────────────────────────

#[tokio::test]
async fn client_stop_recording_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.stop_recording().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "recording");
    assert_eq!(log[0].1["action"], "stop");
}

// ── export_session ────────────────────────────────────────────────────────

#[tokio::test]
async fn client_export_session_sends_correct_params() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client.export_session().await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "recording");
    assert_eq!(log[0].1["action"], "export");
}

// ── find_elements ─────────────────────────────────────────────────────────

#[tokio::test]
async fn client_find_elements_sends_correct_tool() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    let _ = client
        .find_elements(json!({"text": "Submit", "role": "button"}))
        .await;

    let log = state.call_log.lock().await;
    assert_eq!(log[0].0, "find_elements");
    assert_eq!(log[0].1["text"], "Submit");
    assert_eq!(log[0].1["role"], "button");
}

// ── Assertion Helpers ─────────────────────────────────────────────────────

#[test]
fn assert_json_eq_passes_for_matching() {
    let value = json!({"title": "App", "visible": true});
    assert_json_eq(&value, "/title", &json!("App"));
    assert_json_eq(&value, "/visible", &json!(true));
}

#[test]
#[should_panic(expected = "expected")]
fn assert_json_eq_panics_for_mismatch() {
    let value = json!({"title": "App"});
    assert_json_eq(&value, "/title", &json!("Wrong"));
}

#[test]
#[should_panic(expected = "missing")]
fn assert_json_eq_panics_for_missing_pointer() {
    let value = json!({"title": "App"});
    assert_json_eq(&value, "/nonexistent", &json!("x"));
}

#[test]
fn assert_json_truthy_passes_for_truthy_values() {
    let value = json!({"active": true, "name": "test", "count": 42, "items": [1]});
    assert_json_truthy(&value, "/active");
    assert_json_truthy(&value, "/name");
    assert_json_truthy(&value, "/count");
    assert_json_truthy(&value, "/items");
}

#[test]
#[should_panic(expected = "truthy")]
fn assert_json_truthy_panics_for_false() {
    let value = json!({"active": false});
    assert_json_truthy(&value, "/active");
}

#[test]
#[should_panic(expected = "truthy")]
fn assert_json_truthy_panics_for_null() {
    let value = json!({"empty": null});
    assert_json_truthy(&value, "/empty");
}

#[test]
#[should_panic(expected = "truthy")]
fn assert_json_truthy_panics_for_zero() {
    let value = json!({"count": 0});
    assert_json_truthy(&value, "/count");
}

#[test]
#[should_panic(expected = "truthy")]
fn assert_json_truthy_panics_for_empty_string() {
    let value = json!({"name": ""});
    assert_json_truthy(&value, "/name");
}

#[test]
fn assert_no_a11y_violations_passes() {
    let audit = json!({"summary": {"violations": 0, "passes": 12}});
    assert_no_a11y_violations(&audit);
}

#[test]
#[should_panic(expected = "violations")]
fn assert_no_a11y_violations_panics() {
    let audit = json!({"summary": {"violations": 3, "passes": 12}});
    assert_no_a11y_violations(&audit);
}

#[test]
fn assert_performance_budget_passes() {
    let metrics = json!({
        "navigation": {"load_event_ms": 450.0},
        "js_heap": {"used_mb": 12.5}
    });
    assert_performance_budget(&metrics, 1000.0, 50.0);
}

#[test]
#[should_panic(expected = "load event")]
fn assert_performance_budget_panics_on_slow_load() {
    let metrics = json!({
        "navigation": {"load_event_ms": 2000.0},
        "js_heap": {"used_mb": 12.5}
    });
    assert_performance_budget(&metrics, 1000.0, 50.0);
}

#[test]
#[should_panic(expected = "JS heap")]
fn assert_performance_budget_panics_on_high_heap() {
    let metrics = json!({
        "navigation": {"load_event_ms": 450.0},
        "js_heap": {"used_mb": 100.0}
    });
    assert_performance_budget(&metrics, 1000.0, 50.0);
}

#[test]
fn assert_ipc_healthy_passes() {
    let integrity = json!({"healthy": true, "stale_calls": 0, "error_calls": 0});
    assert_ipc_healthy(&integrity);
}

#[test]
#[should_panic(expected = "integrity")]
fn assert_ipc_healthy_panics() {
    let integrity = json!({"healthy": false, "stale_calls": 2, "error_calls": 1});
    assert_ipc_healthy(&integrity);
}

#[test]
fn assert_state_matches_passes() {
    let verification = json!({"passed": true, "divergences": []});
    assert_state_matches(&verification);
}

#[test]
#[should_panic(expected = "verification")]
fn assert_state_matches_panics() {
    let verification = json!({"passed": false, "divergences": [{"field": "title"}]});
    assert_state_matches(&verification);
}

// ── Response Parsing ──────────────────────────────────────────────────────

#[tokio::test]
async fn response_parsed_as_json() {
    let state = MockState::new();
    let port = start_mock_server(state).await;
    let mut client = VictauriClient::connect(port).await.unwrap();

    let result = client.eval_js("1+1").await.unwrap();
    assert!(
        result.is_object() || result.is_string(),
        "result should be parsed JSON"
    );
}

#[tokio::test]
async fn multiple_calls_use_incrementing_ids() {
    let state = MockState::new();
    let port = start_mock_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();

    let _ = client.eval_js("1").await;
    let _ = client.eval_js("2").await;
    let _ = client.eval_js("3").await;

    let log = state.call_log.lock().await;
    assert_eq!(log.len(), 3);
}

// ── Error Handling ────────────────────────────────────────────────────────

#[tokio::test]
async fn json_rpc_error_returns_mcp_error() {
    let state = MockState::new();
    *state.response_override.lock().await = None;
    let port = start_mock_server(state.clone()).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "totally/bogus",
            "params": {}
        }))
        .send()
        .await
        .unwrap();

    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "should return error for unknown method"
    );
}

// ── Auth Token ────────────────────────────────────────────────────────────

#[tokio::test]
async fn connect_with_token_sends_auth_header() {
    #[derive(Clone)]
    struct AuthMockState {
        session_id: String,
        received_auth: Arc<Mutex<Vec<String>>>,
    }

    async fn auth_mock_handler(State(state): State<AuthMockState>, req: Request<Body>) -> Response {
        if let Some(auth) = req.headers().get("Authorization") {
            state
                .received_auth
                .lock()
                .await
                .push(auth.to_str().unwrap_or("").to_string());
        }
        let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
        let method = parsed["method"].as_str().unwrap_or("");
        let id = parsed.get("id").cloned();

        match method {
            "initialize" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": {},
                        "serverInfo": {"name": "mock", "version": "0.1.0"}
                    }
                });
                let mut response = axum::Json(resp).into_response();
                response
                    .headers_mut()
                    .insert("mcp-session-id", state.session_id.parse().unwrap());
                response
            }
            "notifications/initialized" => StatusCode::OK.into_response(),
            _ => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"content": [{"type": "text", "text": "\"ok\""}]}
                });
                axum::Json(resp).into_response()
            }
        }
    }

    let auth_state = AuthMockState {
        session_id: uuid::Uuid::new_v4().to_string(),
        received_auth: Arc::new(Mutex::new(Vec::new())),
    };
    let auth_state_clone = auth_state.clone();

    let app = Router::new()
        .route("/mcp", post(auth_mock_handler))
        .with_state(auth_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let _client = VictauriClient::connect_with_token(port, Some("my-token"))
        .await
        .unwrap();

    let received = auth_state_clone.received_auth.lock().await;
    assert!(!received.is_empty(), "auth header should be sent");
    assert!(
        received[0].contains("my-token"),
        "auth header should contain token"
    );
}

// ── Stale-session (HTTP 422) auto-recovery — issue #2 ─────────────────────
//
// rmcp's StreamableHttpService rejects a tool call with HTTP 422 "expected initialized
// request" whenever the session is stale (the in-app server restarted, the client
// reconnected, or notifications/initialized was missed). The client must re-handshake once
// and retry transparently; if it is still stale, it must surface a clear error pointing at
// the sessionless REST endpoint.

#[derive(Clone)]
struct StaleState {
    /// One-shot: the next `tools/call` returns 422, then clears — a re-initialized retry
    /// succeeds. Simulates a session invalidated by an app/server restart.
    pending_stale: Arc<std::sync::atomic::AtomicBool>,
    /// Persistent: every `tools/call` returns 422 (re-init never recovers).
    always_stale: Arc<std::sync::atomic::AtomicBool>,
    initialize_count: Arc<AtomicU64>,
    toolcall_attempts: Arc<AtomicU64>,
}

impl StaleState {
    fn new() -> Self {
        Self {
            pending_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            always_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            initialize_count: Arc::new(AtomicU64::new(0)),
            toolcall_attempts: Arc::new(AtomicU64::new(0)),
        }
    }
}

async fn stale_session_handler(State(state): State<StaleState>, body: String) -> Response {
    let parsed: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let method = parsed["method"].as_str().unwrap_or("");
    let id = parsed.get("id").cloned();
    match method {
        "initialize" => {
            state.initialize_count.fetch_add(1, Ordering::Relaxed);
            let resp = json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "serverInfo": {"name": "mock", "version": "0"}
                }
            });
            let mut response = axum::Json(resp).into_response();
            // Mint a fresh session id on every handshake.
            response.headers_mut().insert(
                "mcp-session-id",
                uuid::Uuid::new_v4().to_string().parse().unwrap(),
            );
            response
        }
        "notifications/initialized" => StatusCode::OK.into_response(),
        "tools/call" => {
            state.toolcall_attempts.fetch_add(1, Ordering::Relaxed);
            if state.always_stale.load(Ordering::Relaxed)
                || state.pending_stale.swap(false, Ordering::Relaxed)
            {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "expected initialized request",
                )
                    .into_response();
            }
            let resp = json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"content": [{"type": "text", "text": "{\"ok\":true}"}]}
            });
            axum::Json(resp).into_response()
        }
        _ => StatusCode::OK.into_response(),
    }
}

async fn start_stale_session_server(state: StaleState) -> u16 {
    let app = Router::new()
        .route("/mcp", post(stale_session_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

#[tokio::test]
async fn call_tool_auto_recovers_from_stale_422_session() {
    let state = StaleState::new();
    let port = start_stale_session_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();
    assert_eq!(state.initialize_count.load(Ordering::Relaxed), 1);

    // Simulate the server going stale (e.g. the app was rebuilt + relaunched) right before
    // the next tool call.
    state.pending_stale.store(true, Ordering::Relaxed);

    let result = client.call_tool("eval_js", json!({"code": "1+1"})).await;
    assert!(
        result.is_ok(),
        "expected transparent recovery, got: {result:?}"
    );

    // Re-initialized exactly once more (2 handshakes total) and the tool call was retried.
    assert_eq!(state.initialize_count.load(Ordering::Relaxed), 2);
    assert_eq!(state.toolcall_attempts.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn call_tool_recovery_is_bounded_and_reports_rest_fallback() {
    let state = StaleState::new();
    state.always_stale.store(true, Ordering::Relaxed);
    let port = start_stale_session_server(state.clone()).await;
    let mut client = VictauriClient::connect(port).await.unwrap();

    let err = client
        .call_tool("eval_js", json!({"code": "1+1"}))
        .await
        .expect_err("a persistently-stale session must surface an error");
    let msg = err.to_string();
    assert!(msg.contains("stale") || msg.contains("422"), "msg: {msg}");
    assert!(
        msg.contains("api/tools"),
        "error should point to the REST fallback: {msg}"
    );

    // Bounded: exactly one re-initialization (2 handshakes), no infinite loop.
    assert_eq!(state.initialize_count.load(Ordering::Relaxed), 2);
}
