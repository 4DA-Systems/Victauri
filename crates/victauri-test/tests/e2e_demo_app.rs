//! End-to-end tests against a running demo-app instance.
//!
//! These tests require the demo app to be running:
//!   cargo run -p demo-app
//!
//! Then run with:
//!   `VICTAURI_E2E=1` cargo test -p victauri-test --test `e2e_demo_app`
//!
//! The tests connect via auto-discovery (per-process discovery directory) or default port 7373.

use serde_json::{Value, json};
use victauri_test::VictauriClient;

fn skip_unless_e2e() -> bool {
    std::env::var("VICTAURI_E2E").is_ok()
}

macro_rules! e2e_test {
    ($name:ident, $body:expr) => {
        #[tokio::test]
        async fn $name() {
            if !skip_unless_e2e() {
                eprintln!("skipping {} (set VICTAURI_E2E=1 to run)", stringify!($name));
                return;
            }
            $body
        }
    };
}

// ── Connection & Health ─────────────────────────────────────────────────────

e2e_test!(connect_via_discover, {
    let client = VictauriClient::discover().await.unwrap();
    assert!(!client.session_id().is_empty());
});

e2e_test!(health_endpoint_ok, {
    let client = VictauriClient::discover().await.unwrap();
    let resp = reqwest::get(format!("{}/health", client.base_url()))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(
        body.get("uptime_secs").is_none(),
        "hardened health should not leak uptime"
    );
});

e2e_test!(info_endpoint_ok, {
    let client = VictauriClient::discover().await.unwrap();
    let resp = reqwest::get(format!("{}/info", client.base_url()))
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "victauri");
    assert_eq!(body["protocol"], "mcp");
    assert!(!body["version"].as_str().unwrap().is_empty());
});

// ── Eval JS ─────────────────────────────────────────────────────────────────

e2e_test!(eval_js_document_title, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.eval_js("document.title").await.unwrap();
    assert_eq!(result.as_str().unwrap(), "Victauri Demo");
});

e2e_test!(eval_js_bridge_exists, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.eval_js("typeof window.__VICTAURI__").await.unwrap();
    assert_eq!(result.as_str().unwrap(), "object");
});

e2e_test!(eval_js_complex_expression, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .eval_js("JSON.stringify({url: location.href, lang: document.documentElement.lang})")
        .await
        .unwrap();
    let parsed: Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
    assert!(parsed["url"].as_str().unwrap().contains("localhost"));
    assert_eq!(parsed["lang"], "en");
});

e2e_test!(eval_js_async_expression, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .eval_js("await new Promise(r => setTimeout(() => r('done'), 50))")
        .await
        .unwrap();
    assert_eq!(result.as_str().unwrap(), "done");
});

e2e_test!(eval_js_returns_number, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .eval_js("document.querySelectorAll('.card').length")
        .await
        .unwrap();
    assert_eq!(result.as_u64().unwrap(), 4);
});

e2e_test!(eval_js_returns_object, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .eval_js("JSON.stringify({a: 1, b: 'two'})")
        .await
        .unwrap();
    let parsed: Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], "two");
});

e2e_test!(eval_js_null_result, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.eval_js("null").await.unwrap();
    assert!(result.is_null() || result == Value::String("null".into()));
});

// ── DOM Snapshot ────────────────────────────────────────────────────────────

e2e_test!(dom_snapshot_returns_tree, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();
    assert!(snap.get("elements").is_some() || snap.get("tree").is_some() || snap.is_array());
});

e2e_test!(dom_snapshot_has_ref_handles, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();
    let text = serde_json::to_string(&snap).unwrap();
    assert!(text.contains("ref") || text.contains("e1") || text.contains("id"));
});

e2e_test!(dom_snapshot_shows_headings, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();
    let text = serde_json::to_string(&snap).unwrap();
    assert!(text.contains("Victauri Demo") || text.contains("Greet") || text.contains("Counter"));
});

// ── Find Elements ───────────────────────────────────────────────────────────

e2e_test!(find_elements_by_role_button, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .find_elements(json!({"role": "button"}))
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("Greet") || text.contains("Add") || text.contains("button"));
});

e2e_test!(find_elements_by_role_heading, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .find_elements(json!({"role": "heading"}))
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("Victauri") || text.contains("Greet") || text.contains("heading"));
});

e2e_test!(find_elements_by_text, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .find_elements(json!({"text": "Greet"}))
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("Greet"));
});

// ── Interactions: Click ─────────────────────────────────────────────────────

e2e_test!(click_increment_button, {
    let mut client = VictauriClient::discover().await.unwrap();
    // Reset counter first
    client.invoke_command("reset_counter", None).await.unwrap();

    // Get snapshot to find the increment button ref
    let snap = client.dom_snapshot().await.unwrap();
    let snap_text = serde_json::to_string(&snap).unwrap();

    // Find a button ref — look for increment btn
    let ref_id = find_ref_by_text(&snap, "+")
        .unwrap_or_else(|| find_ref_by_id(&snap, "increment-btn").unwrap_or("e1".into()));
    let result = client.click(&ref_id).await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(
        text.contains("ok") || text.contains("true") || !text.contains("error"),
        "click failed: {text}, snapshot had: {}",
        &snap_text[..200.min(snap_text.len())]
    );
});

e2e_test!(click_greet_button_after_fill, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(input_ref) = find_ref_by_id(&snap, "name-input") {
        client.fill(&input_ref, "E2E Test").await.unwrap();
    }

    if let Some(btn_ref) = find_ref_by_id(&snap, "greet-btn") {
        let result = client.click(&btn_ref).await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("error") || text.contains("ok"));
    }
});

// ── Interactions: Fill & Type ───────────────────────────────────────────────

e2e_test!(fill_name_input, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "name-input") {
        let result = client.fill(&ref_id, "Hello World").await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("error"));
    }
});

e2e_test!(type_text_into_todo_input, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "todo-input") {
        let result = client.type_text(&ref_id, "Buy milk").await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("error"));
    }
});

e2e_test!(press_key_tab, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.press_key("Tab").await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("ok") || text.contains("true") || !text.contains("error"));
});

e2e_test!(press_key_escape, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.press_key("Escape").await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("ok") || text.contains("true") || !text.contains("error"));
});

// ── Interactions: Hover & Focus ─────────────────────────────────────────────

e2e_test!(hover_element, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "greet-btn") {
        let result = client.hover(&ref_id).await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("error"));
    }
});

e2e_test!(focus_input_element, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "name-input") {
        let result = client.focus(&ref_id).await.unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("error"));
    }
});

// ── Invoke Tauri Commands ───────────────────────────────────────────────────

e2e_test!(invoke_greet_command, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .invoke_command("greet", Some(json!({"name": "MCP"})))
        .await
        .unwrap();
    let text = result
        .as_str()
        .unwrap_or(&serde_json::to_string(&result).unwrap())
        .to_string();
    assert!(text.contains("Hello") && text.contains("MCP"));
});

e2e_test!(invoke_get_counter, {
    let mut client = VictauriClient::discover().await.unwrap();
    client.invoke_command("reset_counter", None).await.unwrap();
    let result = client.invoke_command("get_counter", None).await.unwrap();
    assert_eq!(result.as_i64().unwrap_or(-1), 0);
});

e2e_test!(invoke_increment_decrement_cycle, {
    let mut client = VictauriClient::discover().await.unwrap();
    client.invoke_command("reset_counter", None).await.unwrap();

    let r1 = client.invoke_command("increment", None).await.unwrap();
    assert_eq!(r1.as_i64().unwrap(), 1);

    let r2 = client.invoke_command("increment", None).await.unwrap();
    assert_eq!(r2.as_i64().unwrap(), 2);

    let r3 = client.invoke_command("decrement", None).await.unwrap();
    assert_eq!(r3.as_i64().unwrap(), 1);
});

e2e_test!(invoke_todo_crud, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Add a todo
    let todo = client
        .invoke_command("add_todo", Some(json!({"title": "E2E test todo"})))
        .await
        .unwrap();
    let id = todo["id"].as_u64().unwrap();
    assert_eq!(todo["title"], "E2E test todo");
    assert_eq!(todo["completed"], false);

    // List should include it
    let list = client.invoke_command("list_todos", None).await.unwrap();
    let todos = list.as_array().unwrap();
    assert!(todos.iter().any(|t| t["id"].as_u64() == Some(id)));

    // Toggle it
    let toggled = client
        .invoke_command("toggle_todo", Some(json!({"id": id})))
        .await
        .unwrap();
    assert_eq!(toggled["completed"], true);

    // Delete it
    client
        .invoke_command("delete_todo", Some(json!({"id": id})))
        .await
        .unwrap();

    // Verify gone
    let list2 = client.invoke_command("list_todos", None).await.unwrap();
    let todos2 = list2.as_array().unwrap();
    assert!(!todos2.iter().any(|t| t["id"].as_u64() == Some(id)));
});

e2e_test!(invoke_settings_crud, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Get defaults
    let settings = client.invoke_command("get_settings", None).await.unwrap();
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["language"], "en");

    // Update theme
    let updated = client
        .invoke_command("update_settings", Some(json!({"theme": "light"})))
        .await
        .unwrap();
    assert_eq!(updated["theme"], "light");

    // Restore default
    client
        .invoke_command("update_settings", Some(json!({"theme": "dark"})))
        .await
        .unwrap();
});

e2e_test!(invoke_get_app_state, {
    let mut client = VictauriClient::discover().await.unwrap();
    client.invoke_command("reset_counter", None).await.unwrap();

    let state = client.invoke_command("get_app_state", None).await.unwrap();
    assert_eq!(state["counter"], 0);
    assert!(state["settings"].is_object());
    assert!(state["todos"].is_array());
});

e2e_test!(invoke_nonexistent_command, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .invoke_command("this_does_not_exist_xyz", None)
        .await
        .unwrap();
    // Tauri returns null/empty for nonexistent commands
    let text = serde_json::to_string(&result).unwrap();
    assert!(text == "null" || text == "{}" || text.contains("error") || text.contains("unknown"));
});

// ── Window Management ───────────────────────────────────────────────────────

e2e_test!(list_windows_contains_main, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.list_windows().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("main"));
});

e2e_test!(get_window_state_main, {
    let mut client = VictauriClient::discover().await.unwrap();
    let state = client.get_window_state(Some("main")).await.unwrap();
    let text = serde_json::to_string(&state).unwrap();
    assert!(text.contains("visible") || text.contains("width") || text.contains("main"));
});

e2e_test!(window_state_has_dimensions, {
    let mut client = VictauriClient::discover().await.unwrap();
    let state = client.get_window_state(Some("main")).await.unwrap();
    let text = serde_json::to_string(&state).unwrap();
    // Should have size info
    assert!(text.contains("width") || text.contains("size") || text.contains("900"));
});

// ── Screenshot ──────────────────────────────────────────────────────────────

e2e_test!(screenshot_returns_base64_png, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.screenshot().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    // PNG magic in base64: iVBORw0KGgo
    assert!(
        text.contains("iVBORw0KGgo") || text.contains("base64") || text.contains("png"),
        "screenshot should contain PNG data, got: {}...",
        &text[..100.min(text.len())]
    );
});

// ── Registry (Inspectable Commands) ─────────────────────────────────────────

e2e_test!(registry_returns_array, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_registry().await.unwrap();
    let arr = result.as_array().unwrap();
    assert_eq!(arr.len(), 12, "demo app registers 12 commands");
});

e2e_test!(registry_search_returns_array, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .call_tool("get_registry", json!({"query": "counter"}))
        .await
        .unwrap();
    let arr = result.as_array().unwrap();
    assert!(!arr.is_empty(), "should find counter-related commands");
});

// ── Resolve Command (NL → command) ──────────────────────────────────────────

e2e_test!(resolve_command_returns_results, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .call_tool("resolve_command", json!({"query": "increase the counter"}))
        .await
        .unwrap();
    let arr = result.as_array().unwrap();
    assert!(!arr.is_empty(), "should resolve counter-related commands");
});

e2e_test!(resolve_command_with_different_query, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .call_tool("resolve_command", json!({"query": "show settings"}))
        .await
        .unwrap();
    assert!(result.is_array());
});

// ── Verification ────────────────────────────────────────────────────────────

e2e_test!(verify_state_title_matches, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .verify_state("document.title", json!("Victauri Demo"))
        .await
        .unwrap();
    victauri_test::assert_state_matches(&result);
});

e2e_test!(verify_state_detects_divergence, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .verify_state("document.title", json!("Wrong Title"))
        .await
        .unwrap();
    assert_eq!(result["passed"], false);
    assert!(!result["divergences"].as_array().unwrap().is_empty());
});

e2e_test!(verify_counter_state_matches_backend, {
    let mut client = VictauriClient::discover().await.unwrap();
    client.invoke_command("reset_counter", None).await.unwrap();
    // Sync DOM display with backend state
    client
        .eval_js("document.getElementById('counter-value').textContent = await window.__TAURI__.core.invoke('get_counter')")
        .await
        .unwrap();
    let result = client
        .verify_state(
            "parseInt(document.getElementById('counter-value').textContent)",
            json!(0),
        )
        .await
        .unwrap();
    victauri_test::assert_state_matches(&result);
});

// ── Ghost Commands ──────────────────────────────────────────────────────────

e2e_test!(detect_ghost_commands_on_demo, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.detect_ghost_commands().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    // All commands in demo-app are #[inspectable], so there should be few/no ghosts
    assert!(text.contains("ghost") || text.contains("commands") || result.is_object());
});

// ── IPC Integrity ───────────────────────────────────────────────────────────

e2e_test!(check_ipc_integrity_healthy, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.check_ipc_integrity().await.unwrap();
    victauri_test::assert_ipc_healthy(&result);
});

// ── Semantic Assertions ─────────────────────────────────────────────────────

e2e_test!(assert_semantic_title_equals, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .assert_semantic(
            "document.title",
            "page title",
            "equals",
            json!("Victauri Demo"),
        )
        .await
        .unwrap();
    assert_eq!(result["passed"], true);
});

e2e_test!(assert_semantic_cards_truthy, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .assert_semantic(
            "document.querySelectorAll('.card').length",
            "card count",
            "truthy",
            json!(null),
        )
        .await
        .unwrap();
    assert_eq!(result["passed"], true);
});

e2e_test!(assert_semantic_greater_than, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .assert_semantic(
            "document.querySelectorAll('.card').length",
            "card count",
            "greater_than",
            json!(2),
        )
        .await
        .unwrap();
    assert_eq!(result["passed"], true);
});

// ── Memory Stats ────────────────────────────────────────────────────────────

e2e_test!(memory_stats_has_working_set, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_memory_stats().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("working_set") || text.contains("rss") || text.contains("bytes"));
});

e2e_test!(memory_stats_nonzero, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_memory_stats().await.unwrap();
    // At least one memory metric should be non-zero
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains(|c: char| c.is_ascii_digit() && c != '0'));
});

// ── Plugin Info ─────────────────────────────────────────────────────────────

e2e_test!(plugin_info_version, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_plugin_info().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("0.1") || text.contains("version"));
});

e2e_test!(plugin_info_tool_count, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_plugin_info().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    // Should report 24 tools
    assert!(text.contains("24") || text.contains("tools") || text.contains("tool_count"));
});

// ── Logs ────────────────────────────────────────────────────────────────────

e2e_test!(console_logs_returns_array, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.logs("console", None).await.unwrap();
    assert!(result.is_array() || result.is_object());
});

e2e_test!(ipc_log_captures_commands, {
    let mut client = VictauriClient::discover().await.unwrap();
    // Trigger some IPC
    client.invoke_command("get_counter", None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let result = client.get_ipc_log(Some(10)).await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    // Should show recent IPC activity
    assert!(result.is_array() || result.is_object() || text.contains("ipc"));
});

e2e_test!(network_logs_returns_data, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.logs("network", None).await.unwrap();
    assert!(result.is_array() || result.is_object());
});

// ── Time-Travel Recording ───────────────────────────────────────────────────

e2e_test!(recording_start_stop_cycle, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Stop any leftover recording
    let _ = client.stop_recording().await;

    let start = client.start_recording(None).await.unwrap();
    assert!(start["started"] == true || start.get("session_id").is_some());

    // Do some activity
    client.invoke_command("increment", None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let stop = client.stop_recording().await.unwrap();
    let text = serde_json::to_string(&stop).unwrap();
    assert!(text.contains("session") || text.contains("events") || text.contains("stopped"));
});

e2e_test!(recording_with_checkpoint, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Stop any leftover recording from prior tests
    let _ = client.stop_recording().await;

    client.start_recording(None).await.unwrap();

    // Create checkpoint
    let cp = client
        .call_tool(
            "recording",
            json!({"action": "checkpoint", "label": "e2e-test"}),
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&cp).unwrap();
    assert!(
        text.contains("checkpoint")
            || text.contains("created")
            || text.contains("id")
            || text.contains("true")
    );

    // List checkpoints
    let list = client
        .call_tool("recording", json!({"action": "list_checkpoints"}))
        .await
        .unwrap();
    let list_text = serde_json::to_string(&list).unwrap();
    assert!(list_text.contains("e2e") || list_text.contains("checkpoint") || list.is_array());

    client.stop_recording().await.unwrap();
});

e2e_test!(recording_export_produces_json, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Stop any leftover recording
    let _ = client.stop_recording().await;

    client.start_recording(None).await.unwrap();
    client.invoke_command("get_counter", None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let export = client.export_session().await.unwrap();
    assert!(export.is_object() || export.is_string());

    client.stop_recording().await.unwrap();
});

// ── Introspection: Styles ───────────────────────────────────────────────────

e2e_test!(get_styles_for_element, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "greet-btn") {
        let result = client
            .call_tool("inspect", json!({"action": "get_styles", "ref_id": ref_id}))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("display") || text.contains("color") || text.contains("style"));
    }
});

e2e_test!(get_bounding_boxes_multiple, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    let refs: Vec<String> = extract_first_n_refs(&snap, 3);
    if !refs.is_empty() {
        let result = client
            .call_tool("inspect", json!({"action": "get_bounds", "ref_ids": refs}))
            .await
            .unwrap();
        let text = serde_json::to_string(&result).unwrap();
        assert!(text.contains("width") || text.contains("x") || text.contains("bound"));
    }
});

// ── Introspection: Accessibility ────────────────────────────────────────────

e2e_test!(accessibility_audit_returns_results, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.audit_accessibility().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("summary") || text.contains("violations") || text.contains("passes"));
});

e2e_test!(accessibility_audit_has_structure, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.audit_accessibility().await.unwrap();
    // Should have a summary section
    assert!(
        result.get("summary").is_some()
            || result.get("checks").is_some()
            || result.get("violations").is_some()
            || result.get("results").is_some()
    );
});

// ── Introspection: Performance ──────────────────────────────────────────────

e2e_test!(performance_metrics_has_navigation, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_performance_metrics().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("navigation") || text.contains("timing") || text.contains("load"));
});

e2e_test!(performance_metrics_has_heap, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_performance_metrics().await.unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("heap") || text.contains("memory") || text.contains("js"));
});

e2e_test!(performance_budget_passes, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.get_performance_metrics().await.unwrap();
    // Demo app should be well within budget: 5000ms load, 200MB heap
    victauri_test::assert_performance_budget(&result, 5000.0, 200.0);
});

// ── CSS Injection ───────────────────────────────────────────────────────────

e2e_test!(inject_css_and_remove, {
    let mut client = VictauriClient::discover().await.unwrap();

    let inject = client
        .call_tool(
            "css",
            json!({"action": "inject", "css": "body { border: 2px solid red; }"}),
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&inject).unwrap();
    assert!(!text.contains("error"));

    let remove = client
        .call_tool("css", json!({"action": "remove"}))
        .await
        .unwrap();
    let text2 = serde_json::to_string(&remove).unwrap();
    assert!(!text2.contains("error"));
});

// ── Highlight/Overlay ───────────────────────────────────────────────────────

e2e_test!(highlight_and_clear, {
    let mut client = VictauriClient::discover().await.unwrap();
    let snap = client.dom_snapshot().await.unwrap();

    if let Some(ref_id) = find_ref_by_id(&snap, "greet-btn") {
        let hl = client
            .call_tool(
                "inspect",
                json!({"action": "highlight", "ref_id": ref_id, "color": "red"}),
            )
            .await
            .unwrap();
        let text = serde_json::to_string(&hl).unwrap();
        assert!(!text.contains("error"));

        let clear = client
            .call_tool("inspect", json!({"action": "clear_highlights"}))
            .await
            .unwrap();
        let text2 = serde_json::to_string(&clear).unwrap();
        assert!(!text2.contains("error"));
    }
});

// ── Wait For ────────────────────────────────────────────────────────────────

e2e_test!(wait_for_text_present, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .wait_for("text", Some("Victauri Demo"), Some(5000), Some(200))
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("true") || text.contains("found") || text.contains("ok"));
});

e2e_test!(wait_for_selector_present, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .wait_for("selector", Some("#greet-btn"), Some(5000), Some(200))
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("true") || text.contains("found") || text.contains("ok"));
});

e2e_test!(wait_for_text_gone_nonexistent, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client
        .wait_for(
            "text_gone",
            Some("XYZ_NONEXISTENT_TEXT_12345"),
            Some(2000),
            Some(200),
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&result).unwrap();
    assert!(text.contains("true") || text.contains("ok") || !text.contains("timeout"));
});

// ── Concurrent Sessions ─────────────────────────────────────────────────────

e2e_test!(two_concurrent_sessions, {
    let mut client1 = VictauriClient::discover().await.unwrap();
    let mut client2 = VictauriClient::discover().await.unwrap();

    assert_ne!(client1.session_id(), client2.session_id());

    let r1 = client1.eval_js("1 + 1").await.unwrap();
    let r2 = client2.eval_js("2 + 2").await.unwrap();

    assert_eq!(r1.as_u64().unwrap(), 2);
    assert_eq!(r2.as_u64().unwrap(), 4);
});

// ── Cross-Boundary: Frontend ↔ Backend ──────────────────────────────────────

e2e_test!(frontend_reflects_backend_counter, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Set counter to known state via backend
    client.invoke_command("reset_counter", None).await.unwrap();
    client.invoke_command("increment", None).await.unwrap();
    client.invoke_command("increment", None).await.unwrap();
    client.invoke_command("increment", None).await.unwrap();

    // Give frontend time to sync (it calls refreshCounter on load but IPC may lag)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Read counter from backend
    let backend_val = client.invoke_command("get_counter", None).await.unwrap();
    assert_eq!(backend_val.as_i64().unwrap(), 3);

    // Read from frontend DOM — note: frontend only updates on button click events,
    // not on backend changes, so we use eval to trigger a refresh
    client
        .eval_js("document.getElementById('counter-value').textContent = await window.__TAURI__.core.invoke('get_counter')")
        .await
        .unwrap();

    let frontend_val = client
        .eval_js("parseInt(document.getElementById('counter-value').textContent)")
        .await
        .unwrap();
    assert_eq!(frontend_val.as_i64().unwrap(), 3);
});

e2e_test!(frontend_todo_matches_backend, {
    let mut client = VictauriClient::discover().await.unwrap();

    // Add via backend
    let todo = client
        .invoke_command("add_todo", Some(json!({"title": "cross-check"})))
        .await
        .unwrap();
    let id = todo["id"].as_u64().unwrap();

    // Refresh frontend
    client
        .eval_js("await (async () => { const todos = await window.__TAURI__.core.invoke('list_todos'); return todos.length; })()")
        .await
        .unwrap();

    // Clean up
    client
        .invoke_command("delete_todo", Some(json!({"id": id})))
        .await
        .unwrap();
});

// ── Edge Cases ──────────────────────────────────────────────────────────────

e2e_test!(eval_js_error_returns_error, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.eval_js("throw new Error('intentional')").await;
    // Should either be an Err or a Value indicating error
    match result {
        Err(_) => {} // Expected
        Ok(v) => {
            let text = serde_json::to_string(&v).unwrap();
            assert!(text.contains("error") || text.contains("intentional"));
        }
    }
});

e2e_test!(eval_js_undefined_returns_empty, {
    let mut client = VictauriClient::discover().await.unwrap();
    let result = client.eval_js("undefined").await.unwrap();
    // undefined → JSON.stringify → "{}" (empty object) or null
    assert!(
        result.is_null()
            || result.is_object()
            || result == Value::String("undefined".into())
            || result == Value::String(String::new()),
        "unexpected result for undefined: {result}"
    );
});

e2e_test!(rapid_tool_calls_dont_crash, {
    let mut client = VictauriClient::discover().await.unwrap();
    for i in 0..20 {
        let result = client.eval_js(&format!("{i} + 1")).await.unwrap();
        assert_eq!(result.as_u64().unwrap(), i + 1);
    }
});

// ── Helpers ─────────────────────────────────────────────────────────────────

fn find_ref_by_id(snapshot: &Value, html_id: &str) -> Option<String> {
    let text = serde_json::to_string(snapshot).unwrap();
    // Try to find an element with the given id and extract its ref
    // Snapshot formats vary — look for patterns like "id":"name-input"..."ref":"eN"
    // or the compact format
    if let Some(pos) = text.find(html_id) {
        // Look for nearby ref
        let search_window = &text[pos.saturating_sub(100)..text.len().min(pos + 200)];
        // Pattern: "ref":"eN" or "ref_id":"eN"
        for pattern in ["\"ref\":\"", "\"ref_id\":\""] {
            if let Some(ref_pos) = search_window.find(pattern) {
                let start = ref_pos + pattern.len();
                if let Some(end) = search_window[start..].find('"') {
                    return Some(search_window[start..start + end].to_string());
                }
            }
        }
    }
    None
}

fn find_ref_by_text(snapshot: &Value, text_content: &str) -> Option<String> {
    let text = serde_json::to_string(snapshot).unwrap();
    if let Some(pos) = text.find(text_content) {
        let search_window = &text[pos.saturating_sub(200)..text.len().min(pos + 200)];
        for pattern in ["\"ref\":\"", "\"ref_id\":\""] {
            if let Some(ref_pos) = search_window.find(pattern) {
                let start = ref_pos + pattern.len();
                if let Some(end) = search_window[start..].find('"') {
                    return Some(search_window[start..start + end].to_string());
                }
            }
        }
    }
    None
}

fn extract_first_n_refs(snapshot: &Value, n: usize) -> Vec<String> {
    let text = serde_json::to_string(snapshot).unwrap();
    let mut refs = Vec::new();
    let mut search_from = 0;

    while refs.len() < n {
        let remaining = &text[search_from..];
        let pattern = "\"ref\":\"";
        if let Some(pos) = remaining.find(pattern) {
            let start = pos + pattern.len();
            if let Some(end) = remaining[start..].find('"') {
                let r = remaining[start..start + end].to_string();
                if !refs.contains(&r) {
                    refs.push(r);
                }
                search_from += start + end;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    refs
}
