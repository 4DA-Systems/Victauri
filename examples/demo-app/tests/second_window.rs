//! Opening a second window from a command, against the real running demo-app.
//!
//! A sync Tauri command runs on the main thread inside the webview's IPC handler. Building a webview
//! there deadlocks on Windows: the window never appears and the calling page stays wedged until the
//! app restarts. So a command that builds a webview must not be sync. The app's frontend does not
//! call `show_notification_window`, so only a test like this one can see that break.
//!
//! Gated by `VICTAURI_E2E=1`; requires the demo-app running. Run:
//! ```sh
//! # build + launch demo-app, then:
//! VICTAURI_E2E=1 cargo test -p demo-app --test second_window -- --test-threads=1
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use victauri_test::{VictauriClient, is_e2e};

/// A tool's text result, or a panic naming what it returned.
async fn call_ok(client: &mut VictauriClient, tool: &str, args: Value) -> String {
    let result = client
        .call_tool_raw(tool, args.clone())
        .await
        .unwrap_or_else(|e| panic!("{tool} {args}: transport error: {e}"));
    assert_ne!(
        result["isError"],
        json!(true),
        "{tool} {args} reported an error: {result}"
    );
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn window_labels(client: &mut VictauriClient) -> String {
    call_ok(client, "window", json!({"action": "list"})).await
}

#[tokio::test]
async fn command_can_open_a_second_window() {
    if !is_e2e() {
        return;
    }
    let mut client = victauri_test::connect()
        .await
        .expect("connect to demo-app (set VICTAURI_E2E=1 and launch the app)");

    // A leftover from an aborted run would turn this into the "show" path only.
    if window_labels(&mut client).await.contains("notifications") {
        call_ok(
            &mut client,
            "window",
            json!({"action": "manage", "manage_action": "close", "label": "notifications"}),
        )
        .await;
    }

    // The bound that matters: a deadlocked command takes the full default eval timeout (30 s).
    let started = std::time::Instant::now();
    call_ok(
        &mut client,
        "invoke_command",
        json!({"command": "show_notification_window", "timeout_ms": 15000}),
    )
    .await;
    println!("opened in {:?}", started.elapsed());

    let labels = window_labels(&mut client).await;
    assert!(labels.contains("notifications"), "window missing: {labels}");

    // The calling page must not be wedged, and the new window's own bridge must answer. It has
    // only just been created, so give it time to load.
    let main = call_ok(
        &mut client,
        "eval_js",
        json!({"code": "return 6*7", "webview_label": "main"}),
    )
    .await;
    assert!(main.contains("42"), "main after opening: {main}");

    let mut second = String::new();
    for _ in 0..30 {
        let attempt = client
            .call_tool_raw(
                "eval_js",
                json!({"code": "return location.pathname", "webview_label": "notifications"}),
            )
            .await
            .expect("transport error reading the second window");
        second = attempt["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if attempt["isError"] != json!(true) && second.contains("notification") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(
        second.contains("notification"),
        "the second window's bridge never answered: {second}"
    );

    // Calling again takes the path that shows the window that already exists.
    call_ok(
        &mut client,
        "invoke_command",
        json!({"command": "show_notification_window", "timeout_ms": 15000}),
    )
    .await;

    call_ok(
        &mut client,
        "window",
        json!({"action": "manage", "manage_action": "close", "label": "notifications"}),
    )
    .await;
}
