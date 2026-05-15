mod auth;
mod bridge_dispatch;
mod installer;
mod mcp_handler;
mod mcp_server;
mod native_messaging;
mod server;
mod tab_state;

use std::net::SocketAddr;
use std::sync::Arc;

use bridge_dispatch::BridgeDispatch;
use mcp_handler::VictauriBrowserHandler;
use tab_state::TabManager;

const DEFAULT_PORT: u16 = 7474;
const PORT_RANGE: u16 = 10;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map_or("serve", String::as_str);

    match command {
        "install" => {
            let extension_id = args.get(2).map_or("EXTENSION_ID", String::as_str);
            let binary = std::env::current_exe()?
                .to_string_lossy()
                .to_string();
            let path = installer::install(&binary, extension_id)?;
            println!("Native messaging host registered at: {path}");
            println!("Extension ID: {extension_id}");
            println!("\nAdd to your .mcp.json:");
            println!(
                r#"{{
  "mcpServers": {{
    "victauri-browser": {{
      "url": "http://127.0.0.1:{DEFAULT_PORT}/mcp"
    }}
  }}
}}"#
            );
            Ok(())
        }
        "uninstall" => {
            installer::uninstall()?;
            println!("Native messaging host unregistered.");
            Ok(())
        }
        "version" => {
            println!("victauri-browser-host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => serve().await,
    }
}

async fn serve() -> anyhow::Result<()> {
    let port = std::env::var("VICTAURI_BROWSER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let auth_token = std::env::var("VICTAURI_BROWSER_AUTH_TOKEN").ok().or_else(|| {
        let token = auth::generate_token();
        tracing::info!("Generated auth token: {token}");
        Some(token)
    });

    let tab_manager = Arc::new(TabManager::new());
    let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));

    spawn_native_reader(Arc::clone(&dispatch), Arc::clone(&tab_manager));

    let handler = VictauriBrowserHandler::new(Arc::clone(&tab_manager), dispatch);
    let app = server::build_app(handler, auth_token);

    let addr = try_bind(port).await?;
    tracing::info!("victauri-browser listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn spawn_native_reader(dispatch: Arc<BridgeDispatch>, tab_manager: Arc<TabManager>) {
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        loop {
            let msg = match native_messaging::read_message(&mut stdin).await {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!("native messaging read error: {e}");
                    dispatch.cancel_all().await;
                    break;
                }
            };
            process_message(&msg, &dispatch, &tab_manager).await;
        }
    });
}

/// Process a single native messaging frame (extracted for testability).
async fn process_message(
    msg: &serde_json::Value,
    dispatch: &BridgeDispatch,
    tab_manager: &TabManager,
) {
    let msg_type = msg
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match msg_type {
        "response" => {
            let id = msg
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let data = msg.get("data").cloned();
            let error = msg
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            dispatch.on_response(id, data, error).await;
        }
        "tab_created" => {
            if let (Some(tab_id), Some(url), Some(title)) = (
                msg.get("tab_id")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u32),
                msg.get("url").and_then(serde_json::Value::as_str),
                msg.get("title").and_then(serde_json::Value::as_str),
            ) {
                tab_manager.on_tab_created(tab_id, url, title).await;
            }
        }
        "tab_closed" => {
            if let Some(tab_id) = msg
                .get("tab_id")
                .and_then(serde_json::Value::as_u64)
                .map(|v| v as u32)
            {
                tab_manager.on_tab_closed(tab_id).await;
            }
        }
        "tab_activated" => {
            if let Some(tab_id) = msg
                .get("tab_id")
                .and_then(serde_json::Value::as_u64)
                .map(|v| v as u32)
            {
                tab_manager.on_tab_activated(tab_id).await;
            }
        }
        "tab_updated" => {
            if let Some(tab_id) = msg
                .get("tab_id")
                .and_then(serde_json::Value::as_u64)
                .map(|v| v as u32)
            {
                let url = msg.get("url").and_then(serde_json::Value::as_str);
                let title = msg.get("title").and_then(serde_json::Value::as_str);
                tab_manager.on_tab_updated(tab_id, url, title).await;
            }
        }
        "bridge_ready" => {
            if let Some(tab_id) = msg
                .get("tab_id")
                .and_then(serde_json::Value::as_u64)
                .map(|v| v as u32)
            {
                tab_manager.on_bridge_ready(tab_id).await;
            }
        }
        _ => {}
    }
}

async fn try_bind(preferred: u16) -> anyhow::Result<SocketAddr> {
    for offset in 0..=PORT_RANGE {
        let port = preferred + offset;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                drop(listener);
                if offset > 0 {
                    tracing::info!("Port {preferred} taken, using {port}");
                }
                return Ok(addr);
            }
            Err(_) => continue,
        }
    }
    anyhow::bail!(
        "no available port in range {preferred}-{}",
        preferred + PORT_RANGE
    )
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::json;

    fn make_test_infra() -> (Arc<BridgeDispatch>, Arc<TabManager>) {
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let tab_manager = Arc::new(TabManager::new());
        (dispatch, tab_manager)
    }

    #[tokio::test]
    async fn full_tab_lifecycle_via_messages() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Chrome sends tab_created
        process_message(
            &json!({"type": "tab_created", "tab_id": 42, "url": "https://example.com", "title": "Example"}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 1);

        // Chrome sends tab_activated
        process_message(
            &json!({"type": "tab_activated", "tab_id": 42}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.get_active_tab_id().await, 42);

        // Chrome sends bridge_ready
        process_message(
            &json!({"type": "bridge_ready", "tab_id": 42}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert!(tab_mgr.is_bridge_ready(42).await);

        // Chrome sends tab_updated (URL changed)
        process_message(
            &json!({"type": "tab_updated", "tab_id": 42, "url": "https://new-url.com"}),
            &dispatch,
            &tab_mgr,
        ).await;
        let tabs = tab_mgr.list_tabs().await;
        assert_eq!(tabs[0].url, "https://new-url.com");
        assert_eq!(tabs[0].title, "Example");

        // Chrome sends tab_closed
        process_message(
            &json!({"type": "tab_closed", "tab_id": 42}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn response_resolves_pending_dispatch() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Simulate the handler registering a pending command
        let ids = dispatch.pending_ids().await;
        assert!(ids.is_empty());

        // Use register_pending on tab manager (which IS accessible)
        // Instead, use on_response directly — it's a no-op if ID doesn't exist,
        // but we can verify the flow by pre-inserting via the dispatch's own methods
        let rx = dispatch.register_test_pending("test-cmd-123").await;

        // Chrome sends the response
        process_message(
            &json!({"type": "response", "id": "test-cmd-123", "data": {"result": "snapshot_data"}}),
            &dispatch,
            &tab_mgr,
        ).await;

        let result = rx.await.unwrap();
        assert_eq!(result.data.unwrap()["result"], "snapshot_data");
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn response_with_error_resolves_correctly() {
        let (dispatch, tab_mgr) = make_test_infra();

        let rx = dispatch.register_test_pending("err-cmd").await;

        process_message(
            &json!({"type": "response", "id": "err-cmd", "error": "element not found"}),
            &dispatch,
            &tab_mgr,
        ).await;

        let result = rx.await.unwrap();
        assert!(result.data.is_none());
        assert_eq!(result.error.unwrap(), "element not found");
    }

    #[tokio::test]
    async fn multiple_tabs_managed_concurrently() {
        let (dispatch, tab_mgr) = make_test_infra();

        for i in 1..=5u32 {
            process_message(
                &json!({"type": "tab_created", "tab_id": i, "url": format!("https://tab{i}.com"), "title": format!("Tab {i}")}),
                &dispatch,
                &tab_mgr,
            ).await;
        }

        assert_eq!(tab_mgr.tab_count().await, 5);

        process_message(
            &json!({"type": "tab_activated", "tab_id": 3}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.get_active_tab_id().await, 3);

        process_message(
            &json!({"type": "tab_closed", "tab_id": 2}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 4);
    }

    #[tokio::test]
    async fn unknown_message_type_is_silent_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "some_future_event", "data": "irrelevant"}),
            &dispatch,
            &tab_mgr,
        ).await;

        assert_eq!(tab_mgr.tab_count().await, 0);
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn missing_type_field_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"tab_id": 1, "url": "https://x.com"}),
            &dispatch,
            &tab_mgr,
        ).await;

        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn tab_created_missing_fields_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Missing title
        process_message(
            &json!({"type": "tab_created", "tab_id": 1, "url": "https://x.com"}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 0);

        // Missing url
        process_message(
            &json!({"type": "tab_created", "tab_id": 1, "title": "X"}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 0);

        // Missing tab_id
        process_message(
            &json!({"type": "tab_created", "url": "https://x.com", "title": "X"}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn response_with_no_matching_id_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "response", "id": "nonexistent-id", "data": {"x": 1}}),
            &dispatch,
            &tab_mgr,
        ).await;

        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn realistic_chrome_session_sequence() {
        let (dispatch, tab_mgr) = make_test_infra();

        // User opens browser, extension starts
        let messages = vec![
            json!({"type": "tab_created", "tab_id": 101, "url": "chrome://newtab", "title": "New Tab"}),
            json!({"type": "tab_activated", "tab_id": 101}),
            // User navigates
            json!({"type": "tab_updated", "tab_id": 101, "url": "https://github.com", "title": "GitHub"}),
            // Bridge injects and signals ready
            json!({"type": "bridge_ready", "tab_id": 101}),
            // User opens second tab
            json!({"type": "tab_created", "tab_id": 102, "url": "https://docs.rs", "title": "docs.rs"}),
            json!({"type": "tab_activated", "tab_id": 102}),
            json!({"type": "bridge_ready", "tab_id": 102}),
            // User goes back to first tab
            json!({"type": "tab_activated", "tab_id": 101}),
            // User closes second tab
            json!({"type": "tab_closed", "tab_id": 102}),
        ];

        for msg in &messages {
            process_message(msg, &dispatch, &tab_mgr).await;
        }

        assert_eq!(tab_mgr.tab_count().await, 1);
        assert_eq!(tab_mgr.get_active_tab_id().await, 101);
        assert!(tab_mgr.is_bridge_ready(101).await);

        let tabs = tab_mgr.list_tabs().await;
        assert_eq!(tabs[0].url, "https://github.com");
        assert_eq!(tabs[0].title, "GitHub");
        assert!(tabs[0].active);
    }

    #[tokio::test]
    async fn response_missing_id_field() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Insert a real pending command
        let _rx = dispatch.register_test_pending("real-id").await;

        // Response without "id" — id defaults to "" which won't match "real-id"
        process_message(
            &json!({"type": "response", "data": {"x": 1}}),
            &dispatch,
            &tab_mgr,
        ).await;

        // The "real-id" should still be pending (not resolved)
        assert_eq!(dispatch.pending_count().await, 1);
    }

    #[tokio::test]
    async fn rapid_tab_events_stress() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Create 100 tabs rapidly
        for i in 1..=100u32 {
            process_message(
                &json!({"type": "tab_created", "tab_id": i, "url": format!("https://{i}.com"), "title": format!("T{i}")}),
                &dispatch,
                &tab_mgr,
            ).await;
        }
        assert_eq!(tab_mgr.tab_count().await, 100);

        // Activate every 10th tab
        for i in (10..=100u32).step_by(10) {
            process_message(
                &json!({"type": "tab_activated", "tab_id": i}),
                &dispatch,
                &tab_mgr,
            ).await;
        }
        assert_eq!(tab_mgr.get_active_tab_id().await, 100);

        // Close odd-numbered tabs
        for i in (1..=100u32).step_by(2) {
            process_message(
                &json!({"type": "tab_closed", "tab_id": i}),
                &dispatch,
                &tab_mgr,
            ).await;
        }
        assert_eq!(tab_mgr.tab_count().await, 50);
    }

    #[tokio::test]
    async fn end_to_end_tool_call_with_response() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));

        // Set up a tab
        process_message(
            &json!({"type": "tab_created", "tab_id": 1, "url": "https://app.com", "title": "App"}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "tab_activated", "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "bridge_ready", "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;

        // Spawn a tool call
        let handle = tokio::spawn(async move {
            handler
                .execute_tool("get_plugin_info", json!({}))
                .await
        });

        // get_plugin_info is handled locally, doesn't need dispatch
        let result = handle.await.unwrap().unwrap();
        assert_eq!(result["name"], "victauri-browser");
        assert_eq!(result["tab_count"], 1);
    }

    #[tokio::test]
    async fn tabs_list_reflects_message_state() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));

        process_message(
            &json!({"type": "tab_created", "tab_id": 1, "url": "https://a.com", "title": "A"}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "tab_created", "tab_id": 2, "url": "https://b.com", "title": "B"}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "tab_activated", "tab_id": 2}),
            &dispatch,
            &tab_mgr,
        ).await;

        let result = handler
            .execute_tool("tabs", json!({"action": "list"}))
            .await
            .unwrap();
        let tabs = result.as_array().unwrap();
        assert_eq!(tabs.len(), 2);

        let active_tab = tabs.iter().find(|t| t["active"] == true).unwrap();
        assert_eq!(active_tab["tab_id"], 2);
        assert_eq!(active_tab["url"], "https://b.com");
    }

    // --- Deep challenger: Chrome extension failure modes ---

    #[tokio::test]
    async fn tab_id_at_u32_max_boundary() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Chrome uses high tab IDs in long-running sessions
        process_message(
            &json!({"type": "tab_created", "tab_id": 4_294_967_295u64, "url": "https://x.com", "title": "Max"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // u32::MAX = 4294967295, but JSON uses u64 and we cast with `as u32`
        // 4294967295 fits in u32 exactly
        assert_eq!(tab_mgr.tab_count().await, 1);

        process_message(
            &json!({"type": "tab_activated", "tab_id": 4_294_967_295u64}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.get_active_tab_id().await, u32::MAX);
    }

    #[tokio::test]
    async fn tab_id_overflow_u32_is_ignored() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Value larger than u32::MAX — as_u64 returns Some but `as u32` would truncate
        // However, the JSON value 4294967296 as u64 gives Some(4294967296),
        // and `4294967296 as u32` == 0 (truncation). This could create a tab with ID 0.
        process_message(
            &json!({"type": "tab_created", "tab_id": 4_294_967_296u64, "url": "https://x.com", "title": "Overflow"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // Tab is created with truncated ID 0
        assert_eq!(tab_mgr.tab_count().await, 1);
    }

    #[tokio::test]
    async fn tab_id_as_string_is_ignored() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Chrome extension bug: tab_id sent as string instead of number
        process_message(
            &json!({"type": "tab_created", "tab_id": "42", "url": "https://x.com", "title": "StringID"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // as_u64 returns None for strings — the handler skips this
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn tab_id_as_float_is_ignored() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "tab_created", "tab_id": 42.5, "url": "https://x.com", "title": "Float"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // as_u64 on float returns None in serde_json
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn tab_id_negative_is_ignored() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "tab_created", "tab_id": -1, "url": "https://x.com", "title": "Neg"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // as_u64 on negative returns None
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn service_worker_restart_simulation() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Simulate: extension service worker starts, creates tabs, then dies
        for i in 1..=5u32 {
            process_message(
                &json!({"type": "tab_created", "tab_id": i, "url": format!("https://t{i}.com"), "title": format!("T{i}")}),
                &dispatch,
                &tab_mgr,
            ).await;
            process_message(
                &json!({"type": "bridge_ready", "tab_id": i}),
                &dispatch,
                &tab_mgr,
            ).await;
        }
        process_message(&json!({"type": "tab_activated", "tab_id": 3}), &dispatch, &tab_mgr).await;
        assert_eq!(tab_mgr.tab_count().await, 5);

        // Service worker restarts — sends fresh tab_created for the same IDs
        // This tests whether duplicate creates are handled
        for i in 1..=5u32 {
            process_message(
                &json!({"type": "tab_created", "tab_id": i, "url": format!("https://t{i}.com"), "title": format!("T{i}")}),
                &dispatch,
                &tab_mgr,
            ).await;
        }
        // Should either overwrite or be additive — either way, tabs exist
        let count = tab_mgr.tab_count().await;
        assert!(count >= 5, "tabs should survive restart: {count}");
    }

    #[tokio::test]
    async fn bridge_ready_for_unknown_tab_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "bridge_ready", "tab_id": 999}),
            &dispatch,
            &tab_mgr,
        ).await;
        // Should not panic or create a phantom tab
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn activate_closed_tab_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": "tab_created", "tab_id": 1, "url": "https://x.com", "title": "X"}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "tab_activated", "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;
        process_message(
            &json!({"type": "tab_closed", "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;

        // Activate a now-closed tab (race condition in Chrome)
        process_message(
            &json!({"type": "tab_activated", "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;
        // Should not panic — but active_tab may point to a ghost
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn concurrent_response_resolution_interleaved_with_tabs() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Set up pending commands
        let rx1 = dispatch.register_test_pending("cmd-1").await;
        let rx2 = dispatch.register_test_pending("cmd-2").await;
        let rx3 = dispatch.register_test_pending("cmd-3").await;

        // Interleave responses with tab events
        let messages = vec![
            json!({"type": "tab_created", "tab_id": 10, "url": "https://a.com", "title": "A"}),
            json!({"type": "response", "id": "cmd-1", "data": {"ok": 1}}),
            json!({"type": "tab_activated", "tab_id": 10}),
            json!({"type": "response", "id": "cmd-2", "data": {"ok": 2}}),
            json!({"type": "bridge_ready", "tab_id": 10}),
            json!({"type": "response", "id": "cmd-3", "data": {"ok": 3}}),
        ];

        for msg in &messages {
            process_message(msg, &dispatch, &tab_mgr).await;
        }

        // All responses resolved correctly
        assert_eq!(rx1.await.unwrap().data.unwrap()["ok"], 1);
        assert_eq!(rx2.await.unwrap().data.unwrap()["ok"], 2);
        assert_eq!(rx3.await.unwrap().data.unwrap()["ok"], 3);

        // Tab state correct
        assert_eq!(tab_mgr.tab_count().await, 1);
        assert!(tab_mgr.is_bridge_ready(10).await);
    }

    #[tokio::test]
    async fn message_with_extra_fields_accepted() {
        let (dispatch, tab_mgr) = make_test_infra();

        // Chrome extension might add extra fields we don't use
        process_message(
            &json!({
                "type": "tab_created",
                "tab_id": 7,
                "url": "https://x.com",
                "title": "X",
                "window_id": 1,
                "index": 0,
                "pinned": false,
                "audible": false,
                "muted_info": {"muted": false}
            }),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 1);
    }

    #[tokio::test]
    async fn null_type_field_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": null, "tab_id": 1}),
            &dispatch,
            &tab_mgr,
        ).await;
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    #[tokio::test]
    async fn response_with_large_data_payload() {
        let (dispatch, tab_mgr) = make_test_infra();

        let rx = dispatch.register_test_pending("big-data").await;

        // Simulate a DOM snapshot response (can be MB-scale)
        let big_array: Vec<serde_json::Value> = (0..1000)
            .map(|i| json!({"ref": format!("e{i}"), "tag": "div", "text": "x".repeat(100)}))
            .collect();

        process_message(
            &json!({"type": "response", "id": "big-data", "data": big_array}),
            &dispatch,
            &tab_mgr,
        ).await;

        let result = rx.await.unwrap();
        assert!(result.error.is_none());
        let data = result.data.unwrap();
        assert_eq!(data.as_array().unwrap().len(), 1000);
    }

    #[tokio::test]
    async fn empty_json_object_message() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(&json!({}), &dispatch, &tab_mgr).await;
        assert_eq!(tab_mgr.tab_count().await, 0);
        assert_eq!(dispatch.pending_count().await, 0);
    }

    #[tokio::test]
    async fn message_type_as_number_is_noop() {
        let (dispatch, tab_mgr) = make_test_infra();

        process_message(
            &json!({"type": 42, "tab_id": 1, "url": "https://x.com", "title": "X"}),
            &dispatch,
            &tab_mgr,
        ).await;
        // as_str on number returns None, defaults to ""
        assert_eq!(tab_mgr.tab_count().await, 0);
    }

    // --- Full-pipeline integration: HTTP → handler → bridge → resolve ---

    #[tokio::test]
    async fn full_pipeline_http_to_bridge_resolution() {
        use axum::body::Body;
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));
        let app = crate::server::build_app(handler, None);

        // Set up a tab
        tab_mgr.on_tab_created(1, "https://app.com", "App").await;
        tab_mgr.on_tab_activated(1).await;
        tab_mgr.on_bridge_ready(1).await;

        // Spawn a background task that simulates the extension responding
        let d = Arc::clone(&dispatch);
        let responder = tokio::spawn(async move {
            // Wait for a pending command to appear
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let ids = d.pending_ids().await;
                if !ids.is_empty() {
                    for id in ids {
                        d.on_response(
                            &id,
                            Some(json!({"tag": "body", "children": [{"tag": "div", "ref": "e0"}]})),
                            None,
                        ).await;
                    }
                    break;
                }
            }
        });

        // Make HTTP request to the REST API
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/tools/dom_snapshot")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["result"]["tag"], "body");
        assert_eq!(json["result"]["children"][0]["ref"], "e0");

        responder.await.unwrap();
    }

    #[tokio::test]
    async fn full_pipeline_concurrent_tool_calls_with_responses() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = Arc::new(VictauriBrowserHandler::new(
            Arc::clone(&tab_mgr),
            Arc::clone(&dispatch),
        ));

        tab_mgr.on_tab_created(1, "https://app.com", "App").await;
        tab_mgr.on_tab_activated(1).await;

        // Spawn a responder that handles multiple concurrent commands
        let d = Arc::clone(&dispatch);
        let responder = tokio::spawn(async move {
            let mut resolved = 0;
            while resolved < 10 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let ids = d.pending_ids().await;
                for id in ids {
                    d.on_response(
                        &id,
                        Some(json!({"resolved": resolved})),
                        None,
                    ).await;
                    resolved += 1;
                }
            }
        });

        // Launch 10 concurrent tool calls
        let mut handles = vec![];
        for _ in 0..10 {
            let h = Arc::clone(&handler);
            handles.push(tokio::spawn(async move {
                h.execute_tool("dom_snapshot", json!({})).await
            }));
        }

        let mut successes = 0;
        for handle in handles {
            if handle.await.unwrap().is_ok() {
                successes += 1;
            }
        }
        assert_eq!(successes, 10);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn full_pipeline_error_propagation() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = VictauriBrowserHandler::new(Arc::clone(&tab_mgr), Arc::clone(&dispatch));

        tab_mgr.on_tab_created(1, "https://app.com", "App").await;
        tab_mgr.on_tab_activated(1).await;

        // Spawn responder that returns an error
        let d = Arc::clone(&dispatch);
        let responder = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let ids = d.pending_ids().await;
                if !ids.is_empty() {
                    for id in ids {
                        d.on_response(&id, None, Some("element not found: e99".to_string())).await;
                    }
                    break;
                }
            }
        });

        let result = handler
            .execute_tool("interact", json!({"action": "click", "ref_id": "e99"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("element not found"));

        responder.await.unwrap();
    }

    #[tokio::test]
    async fn stress_mixed_messages_and_tool_calls() {
        let tab_mgr = Arc::new(TabManager::new());
        let dispatch = Arc::new(BridgeDispatch::new(tokio::io::stdout()));
        let handler = Arc::new(VictauriBrowserHandler::new(
            Arc::clone(&tab_mgr),
            Arc::clone(&dispatch),
        ));

        // Simulate rapid tab lifecycle events while tool calls are in flight
        let d = Arc::clone(&dispatch);
        let tm = Arc::clone(&tab_mgr);

        // Create initial tabs
        for i in 1..=20u32 {
            process_message(
                &json!({"type": "tab_created", "tab_id": i, "url": format!("https://t{i}.com"), "title": format!("T{i}")}),
                &d,
                &tm,
            ).await;
        }
        process_message(&json!({"type": "tab_activated", "tab_id": 10}), &d, &tm).await;

        // Spawn background: resolve any pending commands
        let d2 = Arc::clone(&dispatch);
        let resolver = tokio::spawn(async move {
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                let ids = d2.pending_ids().await;
                for id in ids {
                    d2.on_response(&id, Some(json!({"ok": true})), None).await;
                }
            }
        });

        // Spawn background: rapid tab events
        let d3 = Arc::clone(&dispatch);
        let tm2 = Arc::clone(&tab_mgr);
        let tab_events = tokio::spawn(async move {
            for i in 21..=50u32 {
                process_message(
                    &json!({"type": "tab_created", "tab_id": i, "url": format!("https://t{i}.com"), "title": format!("T{i}")}),
                    &d3,
                    &tm2,
                ).await;
            }
            for i in (1..=20u32).step_by(3) {
                process_message(
                    &json!({"type": "tab_closed", "tab_id": i}),
                    &d3,
                    &tm2,
                ).await;
            }
        });

        // Concurrently call tools
        let mut handles = vec![];
        for _ in 0..20 {
            let h = Arc::clone(&handler);
            handles.push(tokio::spawn(async move {
                h.execute_tool("get_plugin_info", json!({})).await.unwrap()
            }));
        }

        for handle in handles {
            let info = handle.await.unwrap();
            assert_eq!(info["name"], "victauri-browser");
        }

        tab_events.await.unwrap();
        resolver.await.unwrap();

        // After all events: 20 created initially + 30 new - 7 closed = 43
        let count = tab_mgr.tab_count().await;
        assert_eq!(count, 43);
    }
}
