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

    Locator::test_id("increment-btn")
        .click(&mut client)
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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
    // INTEGRITY = round-trip soundness: only *stale* (never-returned) calls are
    // unhealthy. A command that returned an Err is still a healthy round-trip, so
    // `check_ipc_integrity` deliberately lets only stale calls flip `healthy` (see
    // the tool in mcp/mod.rs). The demo-app's own `contact_form_validation_errors`
    // test drives one deliberately-errored frontend IPC call into the shared log,
    // so asserting `error_count == 0` contradicts that contract and fails even on a
    // perfectly sound IPC layer (regression: it red'd E2E on every main push).
    assert!(report["healthy"].as_bool().unwrap());
    assert_eq!(report["stale_count"].as_u64().unwrap(), 0);
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
    if !report.all_passed() {
        let failures: Vec<String> = report
            .failures()
            .iter()
            .map(|f| format!("  {} — {}", f.name, f.detail))
            .collect();
        panic!(
            "smoke test failed: {}/{} passed\n{}",
            report.passed_count(),
            report.total_count(),
            failures.join("\n"),
        );
    }
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

// ────────────────────────────────────────────────────────────────────────────
// Async-completion awareness (Victauri 0.7.6) — await fire-and-forget work
// ────────────────────────────────────────────────────────────────────────────

// `run_pipeline` returns immediately while work runs on a background thread.
// Await its TRUE completion via the `pipeline-complete` Tauri event — no sleeps.
e2e_test!(pipeline_completion_via_event, |client| async move {
    client.invoke_command("run_pipeline", None).await.unwrap();

    let result = client
        .wait_for_event("pipeline-complete", None, Some(5000), Some(50))
        .await
        .unwrap();

    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "event wait should succeed: {result}"
    );
    assert!(
        result["event"]["payload"]
            .as_str()
            .unwrap_or_default()
            .contains("processed"),
        "completion event payload should carry the processed count: {result}"
    );
});

// Await completion the level-triggered way: poll a status command via the
// `expression` condition until the pipeline reports it is no longer running.
e2e_test!(pipeline_completion_via_expression, |client| async move {
    client.invoke_command("run_pipeline", None).await.unwrap();

    let result = client
        .wait_for_expression(
            "(await window.__TAURI_INTERNALS__.invoke('pipeline_status')).running === false",
            None,
            Some(5000),
            Some(50),
        )
        .await
        .unwrap();

    assert_eq!(
        result["ok"],
        serde_json::json!(true),
        "expression wait should resolve once running flips false: {result}"
    );
});

// app_state probe reads the pipeline's backend state directly (no IPC round-trip).
e2e_test!(pipeline_state_via_probe, |client| async move {
    // The probe is always listable.
    let probes = client.app_state(None).await.unwrap();
    assert!(
        serde_json::to_string(&probes).unwrap().contains("pipeline"),
        "app_state should list the 'pipeline' probe: {probes}"
    );

    // Drive the pipeline to completion, then read the probe.
    client.invoke_command("run_pipeline", None).await.unwrap();
    client
        .wait_for_event("pipeline-complete", None, Some(5000), Some(50))
        .await
        .unwrap();

    let state = client.app_state(Some("pipeline")).await.unwrap();
    // `running` is volatile: these e2e tests run in parallel and share the app's
    // single global PipelineState, so another test's concurrent run_pipeline can
    // flip `running` back to true at any instant. Assert the probe exposes the
    // backend snapshot SHAPE (the point of app_state) rather than a racy value.
    assert!(
        state["running"].is_boolean(),
        "probe should expose a boolean running flag: {state}"
    );
    assert_eq!(
        state["pipeline_version"],
        serde_json::json!(5),
        "probe should read the pipeline version from backend state: {state}"
    );
    assert!(
        state["processed"].as_u64().unwrap_or(0) >= 50,
        "pipeline should report processed work (cumulative across runs): {state}"
    );
});

// ────────────────────────────────────────────────────────────────────────────
// Thread-safety regression — webview access must stay on the main (UI) thread
// ────────────────────────────────────────────────────────────────────────────

e2e_test!(
    webview_access_main_thread_safe_under_ipc_load,
    |client| async move {
        // REGRESSION for the Victauri 0.8.0 host-app crash. The MCP server touched the webview
        // `Rc` from its background (axum/tokio) thread — `webview_windows()` / `eval()` — racing
        // the main thread's own non-atomic `Rc` refcounting while the app handled its real IPC
        // (`tauri::ipc::protocol::get`). Concurrent `Rc` mutation corrupts the count → use-after-
        // free (`STATUS_*_BUFFER_OVERRUN`, caught by Rust's debug `assert_unchecked` on
        // `Rc::inc_strong`; "GTK may only be used from the main thread" on Linux). The fix routes
        // ALL webview access through `run_on_main_thread`. This reproduces the race: drive real
        // Tauri IPC from the page (main-thread `Rc`/GTK access) while concurrent clients hammer
        // the webview-touching introspection paths (background-thread access), then assert the
        // host survived. Pre-fix, the background access aborts the process — *deterministically*
        // on Linux/macOS ("only from the main thread") and *probabilistically* on Windows (the
        // `Rc` race). The load is deliberately MODERATE: heavy enough that pre-fix code crashes,
        // light enough that a healthy WebKitGTK (incl. software-rendered xvfb in CI) is not itself
        // overwhelmed — a gate that false-fails on correct code is worse than no gate. See Tauri
        // issue #10001 for the identical crash class.
        use std::time::{Duration, Instant};

        const TASKS: usize = 4;
        const LOAD: Duration = Duration::from_secs(5);

        // 1. Moderate real-IPC flood from the page plus Victauri's legacy async plugin commands
        //    (pre-fix, those commands also cloned the webview map off-thread). 30ms keeps the main
        //    thread genuinely busy with real IPC without saturating the rendering engine.
        client
            .eval_js(
                "(() => { if (!window.__vicLoad) { const invoke = window.__TAURI_INTERNALS__.invoke; \
                window.__vicLoad = setInterval(() => { \
                void invoke('increment').catch(() => {}); \
                void invoke('plugin:victauri|victauri_list_windows').catch(() => {}); \
                void invoke('plugin:victauri|victauri_get_window_state', { label: null }).catch(() => {}); \
                }, 30); } \
                return true; })()",
            )
            .await
            .unwrap();

        // 2. Hammer the webview-touching paths from many concurrent clients (each method takes
        //    `&mut self`, so concurrency needs one client per task). Before the fix, every call
        //    below ran `webview_windows()` on the server's background thread; the fixed path
        //    marshals that access onto the main thread.
        let mut handles = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            handles.push(tokio::spawn(async move {
                let mut c = victauri_test::connect()
                    .await
                    .expect("stress task should connect to the running app");
                let start = Instant::now();
                let mut ops: u64 = 0;
                while start.elapsed() < LOAD {
                    let _ = c.eval_js("document.title").await;
                    let _ = c.get_window_state(None).await;
                    let _ = c.list_windows().await;
                    ops += 1;
                }
                ops
            }));
        }
        let mut ops = 0u64;
        for h in handles {
            ops += h.await.unwrap_or(0);
        }

        // 3. Stop the flood, let the main thread drain, then prove the host is still alive.
        //    Retry to tell a momentarily-saturated-but-healthy app (recovers within a beat) from a
        //    crashed one (persistent connection-refused). A crash = the regression is back.
        let _ = client
            .eval_js("clearInterval(window.__vicLoad); window.__vicLoad = null; true")
            .await;
        let mut alive = false;
        let mut last_err = String::new();
        for _ in 0..10 {
            match client.eval_js("document.title").await {
                Ok(v) if v.is_string() => {
                    alive = true;
                    break;
                }
                Ok(v) => last_err = format!("unexpected eval result: {v:?}"),
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            alive,
            "host app must still respond after concurrent introspection under IPC load — a \
             persistent failure here is the 0.8.0 webview use-after-free crash (pre-fix \
             background-thread webview access). Last error: {last_err}"
        );
        assert!(
            ops > 0,
            "introspection stress loop should have executed at least one cycle"
        );
    }
);
