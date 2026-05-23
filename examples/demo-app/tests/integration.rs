//! Comprehensive integration tests for the Victauri demo app.
//!
//! These tests demonstrate every major Victauri testing pattern:
//! - Direct client API (click, fill, `eval_js`)
//! - Locator API (composable element queries with expectations)
//! - IPC verification (assert commands were called correctly)
//! - Cross-boundary state verification (DOM vs backend)
//! - Accessibility auditing
//! - Performance monitoring
//! - Multi-window testing
//! - Fluent `verify()` builder
//! - Time-travel recording
//!
//! # Running
//!
//! Start the demo app, then run with `VICTAURI_E2E` set:
//! ```sh
//! cd examples/demo-app && cargo tauri dev &
//! VICTAURI_E2E=1 cargo test -p demo-app --test integration
//! ```

use serde_json::json;
use victauri_test::{e2e_test, locator::Locator};

// ────────────────────────────────────────────────────────────────────────────
// Basic interactions — Direct client API
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(greet_flow, |client| async move {
    client.fill_by_id("name-input", "Alice").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    client.expect_text("Hello, Alice!").await.unwrap();
});

e2e_test!(counter_increment, |client| async move {
    // Use return values from increment (each is atomic) to avoid shared-state races
    let v1: i64 =
        serde_json::from_value(client.invoke_command("increment", None).await.unwrap()).unwrap();
    let v2: i64 =
        serde_json::from_value(client.invoke_command("increment", None).await.unwrap()).unwrap();
    let v3: i64 =
        serde_json::from_value(client.invoke_command("increment", None).await.unwrap()).unwrap();

    assert_eq!(v2, v1 + 1, "second increment should be one more than first");
    assert_eq!(v3, v2 + 1, "third increment should be one more than second");
});

e2e_test!(counter_decrement_below_zero, |client| async move {
    // Each decrement returns the new value atomically — assert on relative change only
    let v1: i64 =
        serde_json::from_value(client.invoke_command("decrement", None).await.unwrap()).unwrap();
    let v2: i64 =
        serde_json::from_value(client.invoke_command("decrement", None).await.unwrap()).unwrap();
    assert_eq!(v2, v1 - 1, "second decrement should be one less than first");
});

// ────────────────────────────────────────────────────────────────────────────
// Locator API — Composable element queries
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(locator_greet_by_test_id, |client| async move {
    Locator::test_id("name-input")
        .fill(&mut client, "Charlie")
        .await
        .unwrap();

    Locator::text("Greet").click(&mut client).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let result = client
        .eval_js("document.getElementById('greet-result').textContent")
        .await
        .unwrap();
    let text = result.as_str().unwrap_or("");
    assert!(
        text.contains("Hello") && text.contains("Rust"),
        "greet result should contain greeting from Rust: {text}"
    );
});

e2e_test!(locator_counter_buttons, |client| async move {
    let before: i64 =
        serde_json::from_value(client.invoke_command("get_counter", None).await.unwrap()).unwrap();

    Locator::text("+").click(&mut client).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let after: i64 =
        serde_json::from_value(client.invoke_command("get_counter", None).await.unwrap()).unwrap();
    assert!(after > before, "counter should increase via + button");
});

// ────────────────────────────────────────────────────────────────────────────
// Todo CRUD — Full lifecycle with IPC verification
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(todo_crud_lifecycle, |client| async move {
    // Navigate to todos tab
    Locator::text("Todos").click(&mut client).await.unwrap();

    // Add a todo via IPC
    let todo: serde_json::Value = client
        .invoke_command("add_todo", Some(json!({"title": "Write tests"})))
        .await
        .unwrap();
    let id = todo["id"].as_u64().unwrap() as u32;

    // Verify via backend
    let todos: serde_json::Value = client.invoke_command("list_todos", None).await.unwrap();
    assert!(!todos.as_array().unwrap().is_empty());

    // Toggle completion
    client
        .invoke_command("toggle_todo", Some(json!({"id": id})))
        .await
        .unwrap();

    // Delete
    client
        .invoke_command("delete_todo", Some(json!({"id": id})))
        .await
        .unwrap();

    let remaining: serde_json::Value = client.invoke_command("list_todos", None).await.unwrap();
    let is_gone = !remaining.as_array().unwrap().iter().any(|t| t["id"] == id);
    assert!(is_gone, "todo should be deleted");
});

// ────────────────────────────────────────────────────────────────────────────
// Contact form — Validation patterns
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(contact_form_validation_errors, |client| async move {
    let result = client
        .eval_js(
            "(async () => { \
                try { \
                    await window.__TAURI_INTERNALS__.invoke('submit_contact', \
                        {name:'',email:'bad',message:'hi'}); \
                    return 'no_error'; \
                } catch(e) { return JSON.stringify(e); } \
            })()",
        )
        .await
        .unwrap();

    let fallback = result.to_string();
    let s = result.as_str().unwrap_or(&fallback);
    assert!(
        s.contains("Name is required") || s.contains("field") || s != "no_error",
        "validation errors should propagate: {result}"
    );
});

e2e_test!(contact_form_success, |client| async move {
    let contact: serde_json::Value = client
        .invoke_command(
            "submit_contact",
            Some(json!({
                "name": "Alice Smith",
                "email": "alice@example.com",
                "message": "This is a valid message that is long enough."
            })),
        )
        .await
        .unwrap();

    assert_eq!(contact["name"], "Alice Smith");
    assert_eq!(contact["email"], "alice@example.com");
});

// ────────────────────────────────────────────────────────────────────────────
// Cross-boundary verification — DOM vs Backend state
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(cross_boundary_counter_state, |client| async move {
    // increment returns the new value atomically
    let v1: i64 =
        serde_json::from_value(client.invoke_command("increment", None).await.unwrap()).unwrap();
    let v2: i64 =
        serde_json::from_value(client.invoke_command("increment", None).await.unwrap()).unwrap();
    assert_eq!(v2, v1 + 1, "consecutive increments should differ by 1");

    let report = client.verify().no_console_errors().run().await.unwrap();

    report.assert_all_passed();
});

e2e_test!(settings_cross_boundary, |client| async move {
    client
        .invoke_command("update_settings", Some(json!({"theme": "light"})))
        .await
        .unwrap();

    // Verify the backend state was updated (invoke_command goes through MCP,
    // not the webview fetch interceptor, so it won't appear in the IPC log)
    let state: serde_json::Value = client.invoke_command("get_app_state", None).await.unwrap();
    assert_eq!(
        state["settings"]["theme"], "light",
        "backend state should reflect updated settings"
    );

    // Restore
    client
        .invoke_command("update_settings", Some(json!({"theme": "dark"})))
        .await
        .unwrap();

    let restored: serde_json::Value = client.invoke_command("get_app_state", None).await.unwrap();
    assert_eq!(
        restored["settings"]["theme"], "dark",
        "settings should be restored"
    );
});

// ────────────────────────────────────────────────────────────────────────────
// IPC verification — Integrity and command registry
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(ipc_integrity_check, |client| async move {
    let report = client.check_ipc_integrity().await.unwrap();
    assert!(report["healthy"].as_bool().unwrap());
    assert_eq!(report["error_count"].as_u64().unwrap(), 0);
});

e2e_test!(command_registry_populated, |client| async move {
    let registry = client.get_registry().await.unwrap();
    let commands = registry.as_array().unwrap();

    assert!(
        commands.len() >= 12,
        "expected at least 12 registered commands, got {}",
        commands.len()
    );

    let names: Vec<&str> = commands.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(names.contains(&"greet"));
    assert!(names.contains(&"increment"));
    assert!(names.contains(&"add_todo"));
});

// ────────────────────────────────────────────────────────────────────────────
// Accessibility auditing
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(accessibility_audit, |client| async move {
    let audit = client.audit_accessibility().await.unwrap();
    let violations = audit["summary"]["violations"].as_u64().unwrap_or(0);

    assert!(
        violations < 5,
        "too many a11y violations: {violations}. Details: {}",
        serde_json::to_string_pretty(&audit["violations"]).unwrap_or_default()
    );
});

// ────────────────────────────────────────────────────────────────────────────
// Performance monitoring
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(performance_budget, |client| async move {
    let perf = client.get_performance_metrics().await.unwrap();

    if let Some(dom_interactive) = perf["navigation"]["domInteractive"].as_f64() {
        assert!(
            dom_interactive < 5000.0,
            "DOM interactive took {dom_interactive}ms — should be under 5s"
        );
    }

    if let Some(element_count) = perf["dom"]["elementCount"].as_u64() {
        assert!(
            element_count < 1000,
            "DOM has {element_count} elements — should be under 1000"
        );
    }
});

// ────────────────────────────────────────────────────────────────────────────
// DOM snapshot and window state
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(dom_snapshot_has_elements, |client| async move {
    let snapshot = client.dom_snapshot().await.unwrap();
    let tree = snapshot["tree"].as_str().unwrap_or("");
    assert!(
        tree.contains("body"),
        "snapshot tree should contain body element"
    );
    assert!(
        tree.contains("[e"),
        "snapshot tree should contain ref handles"
    );
});

e2e_test!(window_state_check, |client| async move {
    let windows = client.list_windows().await.unwrap();
    let arr = windows.as_array().unwrap();
    let has_main = arr.iter().any(|w| {
        w.as_str() == Some("main") || w.get("label").and_then(|l| l.as_str()) == Some("main")
    });
    assert!(has_main, "main window should exist");

    let state_val = client.get_window_state(None).await.unwrap();
    let state = if state_val.is_array() {
        &state_val.as_array().unwrap()[0]
    } else {
        &state_val
    };
    assert!(state["visible"].as_bool().unwrap());
    let size = state["size"].as_array().expect("should have size array");
    assert!(size[0].as_f64().unwrap() > 0.0, "width should be > 0");
});

// ────────────────────────────────────────────────────────────────────────────
// Time-travel recording
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(recording_lifecycle, |client| async move {
    let _ = client.stop_recording().await;
    let start = client.start_recording(None).await.unwrap();
    assert!(start["started"].as_bool().unwrap());

    // Do some actions
    client.invoke_command("reset_counter", None).await.unwrap();
    client.invoke_command("increment", None).await.unwrap();

    let session = client.stop_recording().await.unwrap();
    assert!(session.is_object());
});

// ────────────────────────────────────────────────────────────────────────────
// Notification commands — Multi-window state
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(notification_lifecycle, |client| async move {
    let notif: serde_json::Value = client
        .invoke_command(
            "send_notification",
            Some(json!({
                "title": "Test Alert",
                "body": "This is a test notification"
            })),
        )
        .await
        .unwrap();

    assert_eq!(notif["title"], "Test Alert");
    let id = notif["id"].as_u64().unwrap() as u32;

    let count: serde_json::Value = client.invoke_command("unread_count", None).await.unwrap();
    assert!(count.as_u64().unwrap() >= 1);

    client
        .invoke_command("mark_notification_read", Some(json!({"id": id})))
        .await
        .unwrap();

    let all: serde_json::Value = client
        .invoke_command("list_notifications", None)
        .await
        .unwrap();
    let found = all.as_array().unwrap().iter().find(|n| n["id"] == id);
    assert!(found.is_some());
    assert!(found.unwrap()["read"].as_bool().unwrap());
});

// ────────────────────────────────────────────────────────────────────────────
// Smoke test — Built-in comprehensive check
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(smoke_test_suite, |client| async move {
    let report = client.smoke_test().await.unwrap();
    assert!(
        report.all_passed(),
        "smoke test failed: {}/{} passed",
        report.passed_count(),
        report.total_count(),
    );
});

// ────────────────────────────────────────────────────────────────────────────
// Fluent verify() builder — Multiple assertions at once
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(verify_builder_comprehensive, |client| async move {
    let report = client
        .verify()
        .has_text("Victauri Demo")
        .has_no_text("FATAL ERROR")
        .no_console_errors()
        .ipc_was_not_called("delete_account")
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
});
