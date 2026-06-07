mod common;

use std::sync::Arc;

use serde_json::json;
use victauri_core::CommandInfo;
use victauri_plugin::VictauriState;
use victauri_plugin::bridge::WebviewBridge;
use victauri_plugin::mcp::build_app_stateful;

use common::{RejectingMockBridge, test_state};

async fn start_server(state: Arc<VictauriState>, labels: &[&str]) -> String {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(RejectingMockBridge::new(labels));
    let app = build_app_stateful(state, bridge, None);
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
                "clientInfo": {"name": "adversarial-test", "version": "0.1.0"}
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

// =============================================================================
// Group 1: Window Management Adversarial (compound "window" tool)
// =============================================================================

#[tokio::test]
async fn adversarial_manage_window_minimize() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "manage", "manage_action": "minimize"}),
    )
    .await;
    assert!(
        body.contains("minimize executed"),
        "expected 'minimize executed' in: {body}"
    );
}

#[tokio::test]
async fn adversarial_manage_window_invalid_action() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "manage", "manage_action": "explode"}),
    )
    .await;
    assert!(
        body.contains("unknown variant"),
        "expected 'unknown variant' deserialization error in: {body}"
    );
}

#[tokio::test]
async fn adversarial_manage_window_nonexistent_label() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "manage", "manage_action": "minimize", "label": "nonexistent"}),
    )
    .await;
    assert!(
        body.contains("not found"),
        "expected 'not found' error in: {body}"
    );
}

#[tokio::test]
async fn adversarial_resize_window_succeeds() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "resize", "width": 1024, "height": 768}),
    )
    .await;
    assert!(
        body.contains("ok"),
        "expected 'ok' in resize response: {body}"
    );
}

#[tokio::test]
async fn adversarial_resize_window_nonexistent() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "resize", "width": 1024, "height": 768, "label": "fake"}),
    )
    .await;
    assert!(
        body.contains("not found"),
        "expected 'not found' error for nonexistent window: {body}"
    );
}

#[tokio::test]
async fn adversarial_move_window_succeeds() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "move_to", "x": 100, "y": 200}),
    )
    .await;
    assert!(
        body.contains("ok"),
        "expected 'ok' in move response: {body}"
    );
}

#[tokio::test]
async fn adversarial_move_window_nonexistent() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "move_to", "x": 100, "y": 200, "label": "fake"}),
    )
    .await;
    assert!(
        body.contains("not found"),
        "expected 'not found' error for nonexistent window: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_window_title_succeeds() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "set_title", "title": "New Title"}),
    )
    .await;
    assert!(
        body.contains("ok"),
        "expected 'ok' in set_window_title response: {body}"
    );
}

#[tokio::test]
async fn adversarial_set_window_title_nonexistent() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "set_title", "title": "New Title", "label": "fake"}),
    )
    .await;
    assert!(
        body.contains("not found"),
        "expected 'not found' error for nonexistent window: {body}"
    );
}

#[tokio::test]
async fn adversarial_manage_window_all_valid_actions() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let actions = [
        "minimize",
        "unminimize",
        "maximize",
        "unmaximize",
        "close",
        "focus",
        "show",
        "hide",
        "fullscreen",
        "unfullscreen",
        "always_on_top",
        "not_always_on_top",
    ];

    for action in &actions {
        let body = call_tool(
            &client,
            &base,
            &sid,
            "window",
            json!({"action": "manage", "manage_action": action}),
        )
        .await;
        assert!(
            body.contains("executed"),
            "action '{action}' should return 'executed' but got: {body}"
        );
    }
}

// =============================================================================
// Group 2: Recording Tool Adversarial (compound "recording" tool)
// =============================================================================

#[tokio::test]
async fn adversarial_stop_recording_without_start() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
    assert!(
        body.contains("no recording"),
        "expected 'no recording' error when stopping without starting: {body}"
    );
}

#[tokio::test]
async fn adversarial_checkpoint_without_recording() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "cp1", "checkpoint_label": "test checkpoint", "state": {}}),
    )
    .await;
    assert!(
        body.contains("no active recording"),
        "expected 'no active recording' error when checkpointing without recording: {body}"
    );
}

#[tokio::test]
async fn adversarial_checkpoint_auto_generates_id() {
    // Regression: checkpoint_id is optional — when omitted, the handler must
    // auto-generate a `cp-<uuid>` and echo it back (it previously hard-errored
    // with "missing checkpoint_id").
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let _ = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "auto-id-test"}),
    )
    .await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint"}),
    )
    .await;
    assert!(
        body.contains("created") && body.contains("true"),
        "checkpoint without an id must be created: {body}"
    );
    assert!(
        body.contains("checkpoint_id") && body.contains("cp-"),
        "checkpoint without an id must echo back an auto-generated cp-<uuid>: {body}"
    );
}

#[tokio::test]
async fn adversarial_list_checkpoints_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // Start recording first
    let start_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "test-session"}),
    )
    .await;
    assert!(
        start_body.contains("true"),
        "recording should start successfully: {start_body}"
    );

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "list_checkpoints"}),
    )
    .await;
    assert!(
        body.contains("[]"),
        "expected empty array for list_checkpoints on fresh recording: {body}"
    );

    // Cleanup: stop recording
    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn adversarial_get_replay_sequence_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let _ = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "replay-test"}),
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
    assert!(
        body.contains("[]"),
        "expected empty array for get_replay_sequence on fresh recording: {body}"
    );

    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn adversarial_get_recorded_events_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let _ = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "events-test"}),
    )
    .await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "get_events"}),
    )
    .await;
    assert!(
        body.contains("[]"),
        "expected empty array for get_recorded_events on fresh recording: {body}"
    );

    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn adversarial_events_between_invalid_checkpoints() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let _ = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "between-test"}),
    )
    .await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "events_between", "from": "nonexistent_a", "to": "nonexistent_b"}),
    )
    .await;
    assert!(
        body.contains("not found"),
        "expected 'not found' error for nonexistent checkpoints: {body}"
    );

    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

#[tokio::test]
async fn adversarial_export_session_without_recording() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "export"}),
    )
    .await;
    assert!(
        body.contains("no recording"),
        "expected 'no recording' error when exporting without recording: {body}"
    );
}

#[tokio::test]
async fn adversarial_import_session_invalid_json() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "import", "session_json": "not valid json"}),
    )
    .await;
    assert!(
        body.contains("invalid"),
        "expected 'invalid' error for malformed session JSON: {body}"
    );
}

#[tokio::test]
async fn adversarial_import_session_valid() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let session_json = serde_json::to_string(&json!({
        "id": "test-import",
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
    assert!(
        body.contains("imported"),
        "expected 'imported' in response: {body}"
    );
}

#[tokio::test]
async fn adversarial_recording_full_lifecycle() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // 1. Start recording
    let start_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "lifecycle-test"}),
    )
    .await;
    assert!(
        start_body.contains("started") && start_body.contains("true"),
        "recording should start: {start_body}"
    );

    // 2. Create a checkpoint
    let cp_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "checkpoint", "checkpoint_id": "cp-1", "checkpoint_label": "initial", "state": {"counter": 0}}),
    )
    .await;
    assert!(
        cp_body.contains("created") && cp_body.contains("true"),
        "checkpoint should be created: {cp_body}"
    );

    // 3. Get recorded events (should be empty -- no events pushed, only checkpoints)
    let events_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "get_events"}),
    )
    .await;
    assert!(
        events_body.contains("[]"),
        "recorded events should be empty array: {events_body}"
    );

    // 4. Export session (without stopping)
    let export_body = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "export"}),
    )
    .await;
    assert!(
        export_body.contains("lifecycle-test"),
        "exported session should contain session id: {export_body}"
    );

    // 5. Stop recording
    let stop_body = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
    assert!(
        stop_body.contains("lifecycle-test"),
        "stopped session should contain session id: {stop_body}"
    );
    assert!(
        stop_body.contains("cp-1"),
        "stopped session should contain checkpoint id: {stop_body}"
    );

    // 6. Import the exported session back
    let session_json = serde_json::to_string(&json!({
        "id": "lifecycle-test-reimport",
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
async fn adversarial_double_start_recording() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // First start should succeed
    let first = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "first-session"}),
    )
    .await;
    assert!(
        first.contains("started") && first.contains("true"),
        "first start_recording should succeed: {first}"
    );

    // Second start should NOT start (already recording)
    let second = call_tool(
        &client,
        &base,
        &sid,
        "recording",
        json!({"action": "start", "session_id": "second-session"}),
    )
    .await;
    assert!(
        second.contains("already active") || second.contains("isError"),
        "second start_recording should return an error: {second}"
    );

    let _ = call_tool(&client, &base, &sid, "recording", json!({"action": "stop"})).await;
}

// =============================================================================
// Group 3: Protocol Adversarial
// =============================================================================

#[tokio::test]
async fn adversarial_tool_call_missing_required_params() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // eval_js requires "code" parameter
    let body = call_tool(&client, &base, &sid, "eval_js", json!({})).await;
    // The server should return an error (missing required param), not crash
    assert!(
        body.contains("error") || body.contains("Error") || body.contains("missing"),
        "expected error for missing required params in eval_js: {body}"
    );
}

#[tokio::test]
async fn adversarial_unknown_tool_name() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "nonexistent_tool_42", json!({})).await;
    assert!(
        body.contains("error")
            || body.contains("Error")
            || body.contains("not found")
            || body.contains("unknown"),
        "expected error for unknown tool name: {body}"
    );
}

#[tokio::test]
async fn adversarial_malformed_json_body() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body("this is not valid json {{{")
        .send()
        .await
        .unwrap();

    // Server should not crash; should return an error status or error body
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert!(
        status >= 400 || body.contains("error") || body.contains("Error"),
        "expected error for malformed JSON, got status {status}: {body}"
    );
}

#[tokio::test]
async fn adversarial_empty_body() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body("")
        .send()
        .await
        .unwrap();

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    assert!(
        status >= 400 || body.contains("error") || body.contains("Error"),
        "expected error for empty body, got status {status}: {body}"
    );
}

#[tokio::test]
async fn adversarial_huge_tool_args() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    // 50,000 character string -- MockBridge will return an eval error, but no panic
    let huge_code = "x".repeat(50_000);
    let body = call_tool(&client, &base, &sid, "eval_js", json!({"code": huge_code})).await;
    // The tool should handle it (return error since MockBridge rejects eval), not crash
    assert!(
        !body.is_empty(),
        "server should return a response even for huge args, got empty body"
    );
}

#[tokio::test]
async fn adversarial_concurrent_sessions() {
    let state = test_state();
    let base = start_server(state, &["main", "settings"]).await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let base_clone = base.clone();
        let handle = tokio::spawn(async move {
            let (client, sid) = mcp_session(&base_clone).await;
            let body = call_tool(
                &client,
                &base_clone,
                &sid,
                "window",
                json!({"action": "list"}),
            )
            .await;
            (i, body)
        });
        handles.push(handle);
    }

    for handle in handles {
        let (i, body) = handle.await.unwrap();
        assert!(
            body.contains("main"),
            "concurrent session {i} should list 'main' window: {body}"
        );
        assert!(
            body.contains("settings"),
            "concurrent session {i} should list 'settings' window: {body}"
        );
    }
}

// =============================================================================
// Group 4: State/Info Adversarial
// =============================================================================

#[tokio::test]
async fn adversarial_get_plugin_info_returns_structure() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "get_plugin_info", json!({})).await;
    assert!(
        body.contains("version"),
        "get_plugin_info should contain 'version': {body}"
    );
    assert!(
        body.contains("tools"),
        "get_plugin_info should contain 'tools': {body}"
    );
    assert!(
        body.contains("port"),
        "get_plugin_info should contain 'port': {body}"
    );
}

#[tokio::test]
async fn adversarial_get_window_state_specific_label() {
    let state = test_state();
    let base = start_server(state, &["main", "settings"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "get_state", "label": "main"}),
    )
    .await;
    assert!(
        body.contains("main"),
        "get_window_state with label:'main' should return main window: {body}"
    );
    // Should NOT contain the settings window
    // (The response is embedded in SSE, so the main window data should be there)
    assert!(
        body.contains("main title") || body.contains("\"label\":"),
        "get_window_state should return window state data: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_window_state_nonexistent_label() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "window",
        json!({"action": "get_state", "label": "fake"}),
    )
    .await;
    // A specific label matching no window is an error (not a silent empty
    // array, which reads as "success, no state").
    assert!(
        body.contains("window not found") && body.contains("fake"),
        "get_window_state with nonexistent label should return an error: {body}"
    );
}

#[tokio::test]
async fn adversarial_list_windows_multiple() {
    let state = test_state();
    let base = start_server(state, &["main", "settings", "debug"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "window", json!({"action": "list"})).await;
    assert!(
        body.contains("main"),
        "list_windows should contain 'main': {body}"
    );
    assert!(
        body.contains("settings"),
        "list_windows should contain 'settings': {body}"
    );
    assert!(
        body.contains("debug"),
        "list_windows should contain 'debug': {body}"
    );
}

#[tokio::test]
async fn adversarial_get_registry_empty() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "get_registry", json!({})).await;
    assert!(
        body.contains("[]"),
        "get_registry should return empty array when no commands registered: {body}"
    );
}

#[tokio::test]
async fn adversarial_get_registry_with_query() {
    let state = test_state();
    state
        .registry
        .register(CommandInfo::new("test_cmd").with_description("A test command"));

    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "get_registry",
        json!({"query": "test"}),
    )
    .await;
    assert!(
        body.contains("test_cmd"),
        "get_registry with query 'test' should find 'test_cmd': {body}"
    );
}

#[tokio::test]
async fn adversarial_get_memory_stats_returns_valid() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(&client, &base, &sid, "get_memory_stats", json!({})).await;
    // On Windows this returns working_set_bytes, on Linux virtual_size_bytes, etc.
    // Just verify it returns something JSON-like and not an error
    assert!(
        body.contains("bytes") || body.contains("size") || body.contains("memory"),
        "get_memory_stats should return memory data: {body}"
    );
}

#[tokio::test]
async fn adversarial_resolve_command_no_match() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;
    let (client, sid) = mcp_session(&base).await;

    let body = call_tool(
        &client,
        &base,
        &sid,
        "resolve_command",
        json!({"query": "nonexistent_gibberish_xyz_42"}),
    )
    .await;
    assert!(
        body.contains("[]"),
        "resolve_command with no matching commands should return empty array: {body}"
    );
}

// =============================================================================
// Group 5: Health/Info Endpoint Adversarial
// =============================================================================

#[tokio::test]
async fn adversarial_health_returns_minimal_response() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "health endpoint should return 200"
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        json["status"].as_str(),
        Some("ok"),
        "health should contain status=ok: {json}"
    );
    assert!(
        json.get("uptime_secs").is_none(),
        "hardened health endpoint should not leak uptime"
    );
    assert!(
        json.get("memory").is_none(),
        "hardened health endpoint should not leak memory stats"
    );
    assert!(
        json.get("commands_registered").is_none(),
        "hardened health endpoint should not leak command count"
    );
}

#[tokio::test]
async fn adversarial_info_without_session() {
    let state = test_state();
    let base = start_server(state, &["main"]).await;

    let resp = reqwest::get(format!("{base}/info")).await.unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "info endpoint should return 200 without MCP session"
    );
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        json["name"], "victauri",
        "info should have name 'victauri': {json}"
    );
    assert!(
        json["version"].is_string(),
        "info should have string 'version': {json}"
    );
    assert_eq!(
        json["protocol"], "mcp",
        "info should have protocol 'mcp': {json}"
    );
}

#[tokio::test]
async fn adversarial_post_to_health_rejected() {
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

    let status = resp.status().as_u16();
    // POST to a GET-only endpoint should be rejected (405 Method Not Allowed)
    assert!(
        status == 405 || status >= 400,
        "POST to /health should be rejected, got status: {status}"
    );
}
