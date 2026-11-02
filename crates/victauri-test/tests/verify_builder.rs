//! Tests for the fluent verification builder and IPC assertions.
//!
//! Requires a running demo app:
//!   `VICTAURI_PORT=7374` cargo run -p demo-app
//!
//! Then:
//!   `VICTAURI_E2E=1` `VICTAURI_PORT=7374` cargo test -p victauri-test --test `verify_builder`

use victauri_test::{VictauriClient, assert_ipc_called, assert_ipc_not_called};

fn skip_unless_e2e() -> bool {
    std::env::var("VICTAURI_E2E").is_ok()
}

fn port() -> u16 {
    std::env::var("VICTAURI_PORT")
        .unwrap_or_else(|_| "7373".to_string())
        .parse()
        .unwrap()
}

#[tokio::test]
async fn ipc_checkpoint_tracks_new_calls() {
    if !skip_unless_e2e() {
        return;
    }
    let mut client = VictauriClient::connect(port()).await.unwrap();

    let checkpoint = client.create_ipc_checkpoint().await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();

    // Give IPC a moment to be logged
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let calls = client.get_ipc_calls_since(checkpoint).await.unwrap();
    assert!(
        !calls.is_empty(),
        "expected at least one IPC call after click"
    );
}

#[tokio::test]
async fn verify_builder_basic() {
    if !skip_unless_e2e() {
        return;
    }
    let mut client = VictauriClient::connect(port()).await.unwrap();

    let report = client
        .verify()
        .has_no_text("NONEXISTENT_xyz_42")
        .ipc_healthy()
        .no_console_errors()
        .run()
        .await
        .unwrap();

    report.assert_all_passed();
}

#[tokio::test]
async fn verify_builder_reports_failures_without_panic() {
    if !skip_unless_e2e() {
        return;
    }
    let mut client = VictauriClient::connect(port()).await.unwrap();

    let report = client
        .verify()
        .has_text("DEFINITELY_NOT_ON_PAGE_xyz")
        .run()
        .await
        .unwrap();

    assert!(!report.all_passed());
    assert_eq!(report.failures().len(), 1);
    assert!(
        report.failures()[0]
            .detail
            .contains("not found on the page")
    );
}

#[tokio::test]
async fn get_ipc_calls_filters_by_command() {
    if !skip_unless_e2e() {
        return;
    }
    let mut client = VictauriClient::connect(port()).await.unwrap();

    // Trigger a greet command
    client.fill_by_id("name-input", "IpcTest").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let greet_calls = client.get_ipc_calls_for("greet").await.unwrap();
    // Should have at least one greet call
    assert!(!greet_calls.is_empty());
}

#[tokio::test]
async fn standalone_ipc_assertions() {
    if !skip_unless_e2e() {
        return;
    }
    let mut client = VictauriClient::connect(port()).await.unwrap();

    // Trigger a command
    client.fill_by_id("name-input", "AssertTest").await.unwrap();
    client.click_by_id("greet-btn").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let log = client.get_ipc_log(Some(1000)).await.unwrap();
    assert_ipc_called(&log, "greet");
    assert_ipc_not_called(&log, "nonexistent_command_xyz");
}

victauri_test::e2e_test!(macro_based_test, |client| async move {
    client.expect_no_text("NONEXISTENT_xyz_42").await.unwrap();
});
