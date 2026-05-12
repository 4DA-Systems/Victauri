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
    client.invoke_command("reset_counter", None).await.unwrap();

    for _ in 0..3 {
        client.click_by_id("increment-btn").await.unwrap();
    }

    let value: String = serde_json::from_value(
        client.eval_js("document.getElementById('counter-value').textContent").await.unwrap()
    ).unwrap();
    assert_eq!(value, "3");
});

e2e_test!(counter_decrement_below_zero, |client| async move {
    client.invoke_command("reset_counter", None).await.unwrap();
    client.click_by_id("decrement-btn").await.unwrap();
    client.click_by_id("decrement-btn").await.unwrap();

    let value: String = serde_json::from_value(
        client.eval_js("document.getElementById('counter-value').textContent").await.unwrap()
    ).unwrap();
    assert_eq!(value, "-2");
});

// ────────────────────────────────────────────────────────────────────────────
// Locator API — Composable element queries
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(locator_greet_by_test_id, |client| async move {
    Locator::test_id("name-input")
        .fill(&mut client, "Bob")
        .await
        .unwrap();

    Locator::text("Greet")
        .click(&mut client)
        .await
        .unwrap();

    Locator::test_id("greet-result")
        .expect(&mut client)
        .to_contain_text("Hello, Bob!")
        .await
        .unwrap();
});

e2e_test!(locator_counter_buttons, |client| async move {
    client.invoke_command("reset_counter", None).await.unwrap();

    Locator::text("+")
        .click(&mut client)
        .await
        .unwrap();

    Locator::test_id("counter-value")
        .expect(&mut client)
        .to_have_text("1")
        .await
        .unwrap();

    Locator::text("Reset")
        .click(&mut client)
        .await
        .unwrap();

    Locator::test_id("counter-value")
        .expect(&mut client)
        .to_have_text("0")
        .await
        .unwrap();
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
    let todos: serde_json::Value = client
        .invoke_command("list_todos", None)
        .await
        .unwrap();
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

    let remaining: serde_json::Value = client
        .invoke_command("list_todos", None)
        .await
        .unwrap();
    let is_gone = !remaining.as_array().unwrap().iter().any(|t| t["id"] == id);
    assert!(is_gone, "todo should be deleted");
});

// ────────────────────────────────────────────────────────────────────────────
// Contact form — Validation patterns
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(contact_form_validation_errors, |client| async move {
    let result = client
        .invoke_command("submit_contact", Some(json!({
            "name": "",
            "email": "bad",
            "message": "short"
        })))
        .await;

    // Server-side validation rejects invalid input
    assert!(result.is_err(), "invalid contact should be rejected");
});

e2e_test!(contact_form_success, |client| async move {
    let contact: serde_json::Value = client
        .invoke_command("submit_contact", Some(json!({
            "name": "Alice Smith",
            "email": "alice@example.com",
            "message": "This is a valid message that is long enough."
        })))
        .await
        .unwrap();

    assert_eq!(contact["name"], "Alice Smith");
    assert_eq!(contact["email"], "alice@example.com");
});

// ────────────────────────────────────────────────────────────────────────────
// Cross-boundary verification — DOM vs Backend state
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(cross_boundary_counter_state, |client| async move {
    client.invoke_command("reset_counter", None).await.unwrap();

    // Increment via UI
    client.click_by_id("increment-btn").await.unwrap();
    client.click_by_id("increment-btn").await.unwrap();

    // Verify DOM and backend agree
    let report = client.verify()
        .state_matches(
            "parseInt(document.getElementById('counter-value').textContent)",
            json!({"counter": 2}),
        )
        .ipc_was_called("increment")
        .no_console_errors()
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
});

e2e_test!(settings_cross_boundary, |client| async move {
    client
        .invoke_command("update_settings", Some(json!({"theme": "light"})))
        .await
        .unwrap();

    let report = client.verify()
        .ipc_was_called("update_settings")
        .run()
        .await
        .unwrap();
    report.assert_all_passed();

    // Verify via get_app_state
    let state: serde_json::Value = client
        .invoke_command("get_app_state", None)
        .await
        .unwrap();
    assert_eq!(state["settings"]["theme"], "light");

    // Restore
    client
        .invoke_command("update_settings", Some(json!({"theme": "dark"})))
        .await
        .unwrap();
});

// ────────────────────────────────────────────────────────────────────────────
// IPC verification — Integrity and command registry
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(ipc_integrity_check, |client| async move {
    let report = client.check_ipc_integrity().await.unwrap();
    assert!(report["healthy"].as_bool().unwrap());
    assert_eq!(report["errored"].as_u64().unwrap(), 0);
});

e2e_test!(command_registry_populated, |client| async move {
    let registry = client.get_registry().await.unwrap();
    let commands = registry.as_array().unwrap();

    assert!(
        commands.len() >= 12,
        "expected at least 12 registered commands, got {}",
        commands.len()
    );

    let names: Vec<&str> = commands
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
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
    let body = &snapshot["body"];
    assert!(body.is_object(), "snapshot should have a body");
});

e2e_test!(window_state_check, |client| async move {
    let windows = client.list_windows().await.unwrap();
    let labels: Vec<&str> = windows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(labels.contains(&"main"), "main window should exist");

    let state = client.get_window_state(None).await.unwrap();
    assert!(state["visible"].as_bool().unwrap());
    assert!(state["width"].as_f64().unwrap() > 0.0);
});

// ────────────────────────────────────────────────────────────────────────────
// Time-travel recording
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(recording_lifecycle, |client| async move {
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
        .invoke_command("send_notification", Some(json!({
            "title": "Test Alert",
            "body": "This is a test notification"
        })))
        .await
        .unwrap();

    assert_eq!(notif["title"], "Test Alert");
    let id = notif["id"].as_u64().unwrap() as u32;

    let count: serde_json::Value = client
        .invoke_command("unread_count", None)
        .await
        .unwrap();
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
    let report = client.verify()
        .has_text("Victauri Demo")
        .has_no_text("FATAL ERROR")
        .no_console_errors()
        .ipc_was_not_called("delete_account")
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
});
