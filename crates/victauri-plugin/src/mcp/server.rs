use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::DefaultBodyLimit;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tauri::Runtime;
use tower::limit::ConcurrencyLimitLayer;

use crate::VictauriState;
use crate::bridge::WebviewBridge;

use super::{MAX_PENDING_EVALS, VictauriMcpHandler};

const DEFAULT_WEBVIEW_LABEL: &str = "main";

// ── Server startup ───────────────────────────────────────────────────────────

/// Build an Axum router for the MCP server with default options (no auth token).
pub fn build_app(state: Arc<VictauriState>, bridge: Arc<dyn WebviewBridge>) -> axum::Router {
    build_app_with_options(state, bridge, None)
}

/// Build an Axum router for the MCP server with an optional auth token and rate limiter.
pub fn build_app_with_options(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    auth_token: Option<String>,
) -> axum::Router {
    build_app_full(state, bridge, auth_token, None)
}

/// Build an Axum router with full control over auth token and rate limiter.
pub fn build_app_full(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    auth_token: Option<String>,
    rate_limiter: Option<Arc<crate::auth::RateLimiterState>>,
) -> axum::Router {
    let handler = VictauriMcpHandler::new(state.clone(), bridge);
    let rest = super::rest::router(handler.clone());

    let mcp_service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let auth_state = Arc::new(crate::auth::AuthState {
        token: auth_token.clone(),
    });
    let info_state = state.clone();
    let info_auth = auth_token.is_some();

    let privacy_enabled = !state.privacy.disabled_tools.is_empty()
        || state.privacy.command_allowlist.is_some()
        || !state.privacy.command_blocklist.is_empty()
        || state.privacy.redaction_enabled;

    let mut router = axum::Router::new()
        .route_service("/mcp", mcp_service)
        .nest("/api/tools", rest)
        .route(
            "/info",
            axum::routing::get(move || {
                let s = info_state.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "name": "victauri",
                        "version": env!("CARGO_PKG_VERSION"),
                        "protocol": "mcp",
                        "commands_registered": s.registry.count(),
                        "events_captured": s.event_log.len(),
                        "port": s.port.load(Ordering::Relaxed),
                        "auth_required": info_auth,
                        "privacy_mode": privacy_enabled,
                    }))
                }
            }),
        );

    if auth_token.is_some() {
        router = router.layer(axum::middleware::from_fn_with_state(
            auth_state,
            crate::auth::require_auth,
        ));
    }

    let limiter = rate_limiter.unwrap_or_else(crate::auth::default_rate_limiter);
    router = router.layer(axum::middleware::from_fn_with_state(
        limiter,
        crate::auth::rate_limit,
    ));

    router
        .route(
            "/health",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"status": "ok"})) }),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(ConcurrencyLimitLayer::new(64))
        .layer(axum::middleware::from_fn(crate::auth::security_headers))
        .layer(axum::middleware::from_fn(crate::auth::origin_guard))
        .layer(axum::middleware::from_fn(crate::auth::dns_rebinding_guard))
}

#[doc(hidden)]
#[allow(dead_code)]
pub mod tests_support {
    /// Expose memory stats for integration tests.
    #[must_use]
    pub fn get_memory_stats() -> serde_json::Value {
        crate::memory::current_stats()
    }
}

const PORT_FALLBACK_RANGE: u16 = 10;

/// Start the MCP server on the given port with default options (no auth token).
///
/// # Errors
///
/// Returns an error if the server fails to bind to the requested port (or any port in the
/// fallback range), or if the server exits unexpectedly.
pub async fn start_server<R: Runtime>(
    app_handle: tauri::AppHandle<R>,
    state: Arc<VictauriState>,
    port: u16,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    start_server_with_options(app_handle, state, port, None, shutdown_rx).await
}

/// Start the MCP server on the given port with an optional auth token.
///
/// # Errors
///
/// Returns an error if the server fails to bind to the requested port (or any port in the
/// fallback range), or if the server exits unexpectedly.
pub async fn start_server_with_options<R: Runtime>(
    app_handle: tauri::AppHandle<R>,
    state: Arc<VictauriState>,
    port: u16,
    auth_token: Option<String>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let bridge: Arc<dyn WebviewBridge> = Arc::new(app_handle);
    let token_for_file = auth_token.clone();
    let app = build_app_with_options(state.clone(), bridge.clone(), auth_token);

    let (listener, actual_port) = try_bind(port).await?;

    if actual_port != port {
        tracing::warn!("Victauri: port {port} in use, fell back to {actual_port}");
    }

    state.port.store(actual_port, Ordering::Relaxed);
    write_port_file(actual_port);
    if let Some(ref token) = token_for_file {
        write_token_file(token);
    }

    tracing::info!("Victauri MCP server listening on 127.0.0.1:{actual_port}");

    let drain_state = state.clone();
    let drain_bridge = bridge;
    let drain_shutdown = state.shutdown_tx.subscribe();
    tokio::spawn(event_drain_loop(drain_state, drain_bridge, drain_shutdown));

    let mut shutdown_rx2 = shutdown_rx.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = shutdown_rx.wait_for(|&v| v).await;
        remove_port_file();
        tracing::info!("Victauri MCP server shutting down gracefully");
    });

    tokio::select! {
        result = server => {
            if let Err(e) = result {
                tracing::error!("Victauri MCP server error: {e}");
            }
        }
        _ = async {
            let _ = shutdown_rx2.wait_for(|&v| v).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        } => {
            tracing::warn!("Victauri MCP server shutdown timeout — forcing exit");
        }
    }
    Ok(())
}

async fn try_bind(preferred: u16) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    if let Ok(listener) = tokio::net::TcpListener::bind(format!("127.0.0.1:{preferred}")).await {
        return Ok((listener, preferred));
    }

    for offset in 1..=PORT_FALLBACK_RANGE {
        let port = preferred + offset;
        if let Ok(listener) = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await {
            return Ok((listener, port));
        }
    }

    anyhow::bail!(
        "could not bind to any port in range {preferred}-{}",
        preferred + PORT_FALLBACK_RANGE
    )
}

fn discovery_dir() -> std::path::PathBuf {
    std::env::temp_dir()
        .join("victauri")
        .join(std::process::id().to_string())
}

fn write_port_file(port: u16) {
    let dir = discovery_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let port_path = dir.join("port");
    if let Err(e) = std::fs::write(&port_path, port.to_string()) {
        tracing::debug!("could not write port file: {e}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&port_path, std::fs::Permissions::from_mode(0o600));
    }
    // Write metadata for multi-server discovery
    let metadata = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "started_at": chrono::Utc::now().to_rfc3339(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    let meta_path = dir.join("metadata.json");
    let _ = std::fs::write(&meta_path, metadata.to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&meta_path, std::fs::Permissions::from_mode(0o600));
    }
}

fn write_token_file(token: &str) {
    let dir = discovery_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let token_path = dir.join("token");
    if let Err(e) = std::fs::write(&token_path, token) {
        tracing::debug!("could not write token file: {e}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600));
    }
}

fn remove_port_file() {
    let _ = std::fs::remove_dir_all(discovery_dir());
}

/// Parse a single bridge event JSON value into an [`AppEvent`](victauri_core::AppEvent).
///
/// Returns `None` for unrecognised event types, allowing callers to skip them.
#[must_use]
pub fn parse_bridge_event(ev: &serde_json::Value) -> Option<victauri_core::AppEvent> {
    use chrono::Utc;
    use victauri_core::AppEvent;

    let event_type = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let now = Utc::now();

    let app_event = match event_type {
        "console" => AppEvent::StateChange {
            key: format!(
                "console.{}",
                ev.get("level").and_then(|l| l.as_str()).unwrap_or("log")
            ),
            timestamp: now,
            caused_by: ev
                .get("message")
                .and_then(|m| m.as_str())
                .map(std::string::ToString::to_string),
        },
        "dom_mutation" => AppEvent::DomMutation {
            webview_label: DEFAULT_WEBVIEW_LABEL.to_string(),
            timestamp: now,
            mutation_count: ev
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32,
        },
        "ipc" => {
            let cmd = ev
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("unknown");
            AppEvent::Ipc(victauri_core::IpcCall {
                id: uuid::Uuid::new_v4().to_string(),
                command: cmd.to_string(),
                timestamp: now,
                result: match ev.get("status").and_then(|s| s.as_str()) {
                    Some("ok") => victauri_core::IpcResult::Ok(serde_json::Value::Null),
                    Some("error") => victauri_core::IpcResult::Err("error".to_string()),
                    _ => victauri_core::IpcResult::Pending,
                },
                duration_ms: ev
                    .get("duration_ms")
                    .and_then(serde_json::Value::as_f64)
                    .map(|d| d as u64),
                arg_size_bytes: 0,
                webview_label: DEFAULT_WEBVIEW_LABEL.to_string(),
            })
        }
        "network" => AppEvent::StateChange {
            key: format!(
                "network.{}",
                ev.get("method").and_then(|m| m.as_str()).unwrap_or("GET")
            ),
            timestamp: now,
            caused_by: ev
                .get("url")
                .and_then(|u| u.as_str())
                .map(std::string::ToString::to_string),
        },
        "navigation" => AppEvent::WindowEvent {
            label: DEFAULT_WEBVIEW_LABEL.to_string(),
            event: format!(
                "navigation.{}",
                ev.get("nav_type")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
            ),
            timestamp: now,
        },
        "dom_interaction" => {
            let action_str = ev.get("action").and_then(|a| a.as_str()).unwrap_or("click");
            let action = match action_str {
                "click" => victauri_core::InteractionKind::Click,
                "double_click" => victauri_core::InteractionKind::DoubleClick,
                "fill" => victauri_core::InteractionKind::Fill,
                "key_press" => victauri_core::InteractionKind::KeyPress,
                "select" => victauri_core::InteractionKind::Select,
                "navigate" => victauri_core::InteractionKind::Navigate,
                "scroll" => victauri_core::InteractionKind::Scroll,
                _ => victauri_core::InteractionKind::Click,
            };
            AppEvent::DomInteraction {
                action,
                selector: ev
                    .get("selector")
                    .and_then(|s| s.as_str())
                    .unwrap_or("body")
                    .to_string(),
                value: ev
                    .get("value")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string),
                timestamp: now,
                webview_label: DEFAULT_WEBVIEW_LABEL.to_string(),
            }
        }
        _ => return None,
    };

    Some(app_event)
}

async fn event_drain_loop(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut last_drain_ts: f64 = 0.0;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            _ = shutdown.changed() => break,
        }

        if !state.recorder.is_recording() {
            continue;
        }

        let code = format!("return window.__VICTAURI__?.getEventStream({last_drain_ts})");
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut pending = state.pending_evals.lock().await;
            if pending.len() >= MAX_PENDING_EVALS {
                continue;
            }
            pending.insert(id.clone(), tx);
        }

        let inject = format!(
            r"
            (async () => {{
                try {{
                    const __result = await (async () => {{ {code} }})();
                    await window.__TAURI__.core.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: '{id}',
                        result: JSON.stringify(__result)
                    }});
                }} catch (e) {{
                    await window.__TAURI__.core.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: '{id}',
                        result: JSON.stringify({{ __error: e.message }})
                    }});
                }}
            }})();
            "
        );

        if bridge.eval_webview(None, &inject).is_err() {
            state.pending_evals.lock().await.remove(&id);
            continue;
        }

        let Ok(Ok(result)) = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await
        else {
            state.pending_evals.lock().await.remove(&id);
            continue;
        };

        let events: Vec<serde_json::Value> = match serde_json::from_str(&result) {
            Ok(v) => v,
            Err(_) => continue,
        };

        for ev in &events {
            let ts = ev
                .get("timestamp")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            if ts > last_drain_ts {
                last_drain_ts = ts;
            }

            if let Some(app_event) = parse_bridge_event(ev) {
                state.recorder.record_event(app_event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use victauri_core::{AppEvent, InteractionKind, IpcResult};

    #[tokio::test]
    async fn try_bind_preferred_port_available() {
        let (listener, port) = try_bind(0).await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(port, 0);
        assert_ne!(addr.port(), 0); // OS assigned a real port
    }

    #[tokio::test]
    async fn try_bind_falls_back_when_taken() {
        let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let blocked_port = blocker.local_addr().unwrap().port();

        let (_, actual) = try_bind(blocked_port).await.unwrap();
        assert_ne!(actual, blocked_port);
        assert!(actual > blocked_port);
        assert!(actual <= blocked_port + PORT_FALLBACK_RANGE);
    }

    #[test]
    fn port_file_roundtrip() {
        write_port_file(7777);
        let dir = discovery_dir();
        let content = std::fs::read_to_string(dir.join("port")).unwrap();
        assert_eq!(content, "7777");
        // Metadata file written
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(meta["port"], 7777);
        assert_eq!(meta["pid"], std::process::id());
        remove_port_file();
        assert!(!dir.exists());
    }

    // ── parse_bridge_event: dom_interaction ────────────────────────────────

    #[test]
    fn parse_dom_interaction_click() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "action": "click",
            "selector": "#submit-btn",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction {
                action,
                selector,
                value,
                webview_label,
                ..
            } => {
                assert_eq!(action, InteractionKind::Click);
                assert_eq!(selector, "#submit-btn");
                assert!(value.is_none());
                assert_eq!(webview_label, "main");
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_fill_with_value() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "action": "fill",
            "selector": "input[name=email]",
            "value": "test@example.com",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction {
                action,
                selector,
                value,
                ..
            } => {
                assert_eq!(action, InteractionKind::Fill);
                assert_eq!(selector, "input[name=email]");
                assert_eq!(value.as_deref(), Some("test@example.com"));
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_key_press() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "action": "key_press",
            "selector": "body",
            "value": "Enter",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction { action, value, .. } => {
                assert_eq!(action, InteractionKind::KeyPress);
                assert_eq!(value.as_deref(), Some("Enter"));
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_unknown_action_defaults_to_click() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "action": "swipe_left",
            "selector": ".card",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction { action, .. } => {
                assert_eq!(action, InteractionKind::Click);
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_missing_action_defaults_to_click() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "selector": "button",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction { action, .. } => {
                assert_eq!(action, InteractionKind::Click);
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_missing_selector_defaults_to_body() {
        let ev = serde_json::json!({
            "type": "dom_interaction",
            "action": "scroll",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomInteraction {
                action, selector, ..
            } => {
                assert_eq!(action, InteractionKind::Scroll);
                assert_eq!(selector, "body");
            }
            other => panic!("expected DomInteraction, got {other:?}"),
        }
    }

    #[test]
    fn parse_dom_interaction_all_action_kinds() {
        let cases = [
            ("click", InteractionKind::Click),
            ("double_click", InteractionKind::DoubleClick),
            ("fill", InteractionKind::Fill),
            ("key_press", InteractionKind::KeyPress),
            ("select", InteractionKind::Select),
            ("navigate", InteractionKind::Navigate),
            ("scroll", InteractionKind::Scroll),
        ];
        for (action_str, expected_kind) in cases {
            let ev = serde_json::json!({
                "type": "dom_interaction",
                "action": action_str,
                "selector": "body",
            });
            let result = parse_bridge_event(&ev)
                .unwrap_or_else(|| panic!("should produce event for action {action_str}"));
            match result {
                AppEvent::DomInteraction { action, .. } => {
                    assert_eq!(action, expected_kind, "mismatch for action {action_str}");
                }
                other => panic!("expected DomInteraction for {action_str}, got {other:?}"),
            }
        }
    }

    // ── parse_bridge_event: ipc ────────────────────────────────────────────

    #[test]
    fn parse_ipc_status_ok() {
        let ev = serde_json::json!({
            "type": "ipc",
            "command": "greet",
            "status": "ok",
            "duration_ms": 42.0,
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::Ipc(call) => {
                assert_eq!(call.command, "greet");
                assert_eq!(call.result, IpcResult::Ok(serde_json::Value::Null));
                assert_eq!(call.duration_ms, Some(42));
                assert_eq!(call.webview_label, "main");
            }
            other => panic!("expected Ipc, got {other:?}"),
        }
    }

    #[test]
    fn parse_ipc_status_error() {
        let ev = serde_json::json!({
            "type": "ipc",
            "command": "save_file",
            "status": "error",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::Ipc(call) => {
                assert_eq!(call.command, "save_file");
                assert_eq!(call.result, IpcResult::Err("error".to_string()));
            }
            other => panic!("expected Ipc, got {other:?}"),
        }
    }

    #[test]
    fn parse_ipc_status_pending() {
        let ev = serde_json::json!({
            "type": "ipc",
            "command": "long_task",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::Ipc(call) => {
                assert_eq!(call.result, IpcResult::Pending);
                assert!(call.duration_ms.is_none());
            }
            other => panic!("expected Ipc, got {other:?}"),
        }
    }

    // ── parse_bridge_event: console ────────────────────────────────────────

    #[test]
    fn parse_console_event() {
        let ev = serde_json::json!({
            "type": "console",
            "level": "warn",
            "message": "deprecated API usage",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::StateChange { key, caused_by, .. } => {
                assert_eq!(key, "console.warn");
                assert_eq!(caused_by.as_deref(), Some("deprecated API usage"));
            }
            other => panic!("expected StateChange, got {other:?}"),
        }
    }

    #[test]
    fn parse_console_default_level() {
        let ev = serde_json::json!({
            "type": "console",
            "message": "hello",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::StateChange { key, .. } => {
                assert_eq!(key, "console.log");
            }
            other => panic!("expected StateChange, got {other:?}"),
        }
    }

    // ── parse_bridge_event: navigation ─────────────────────────────────────

    #[test]
    fn parse_navigation_event() {
        let ev = serde_json::json!({
            "type": "navigation",
            "nav_type": "push",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::WindowEvent { label, event, .. } => {
                assert_eq!(label, "main");
                assert_eq!(event, "navigation.push");
            }
            other => panic!("expected WindowEvent, got {other:?}"),
        }
    }

    #[test]
    fn parse_navigation_default_nav_type() {
        let ev = serde_json::json!({ "type": "navigation" });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::WindowEvent { event, .. } => {
                assert_eq!(event, "navigation.unknown");
            }
            other => panic!("expected WindowEvent, got {other:?}"),
        }
    }

    // ── parse_bridge_event: dom_mutation ───────────────────────────────────

    #[test]
    fn parse_dom_mutation_event() {
        let ev = serde_json::json!({
            "type": "dom_mutation",
            "count": 15,
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::DomMutation {
                webview_label,
                mutation_count,
                ..
            } => {
                assert_eq!(webview_label, "main");
                assert_eq!(mutation_count, 15);
            }
            other => panic!("expected DomMutation, got {other:?}"),
        }
    }

    // ── parse_bridge_event: network ────────────────────────────────────────

    #[test]
    fn parse_network_event() {
        let ev = serde_json::json!({
            "type": "network",
            "method": "POST",
            "url": "https://api.example.com/data",
        });
        let result = parse_bridge_event(&ev).expect("should produce an event");
        match result {
            AppEvent::StateChange { key, caused_by, .. } => {
                assert_eq!(key, "network.POST");
                assert_eq!(caused_by.as_deref(), Some("https://api.example.com/data"));
            }
            other => panic!("expected StateChange, got {other:?}"),
        }
    }

    // ── parse_bridge_event: unknown type ───────────────────────────────────

    #[test]
    fn parse_unknown_type_returns_none() {
        let ev = serde_json::json!({
            "type": "custom_telemetry",
            "payload": 42,
        });
        assert!(parse_bridge_event(&ev).is_none());
    }

    #[test]
    fn parse_missing_type_field_returns_none() {
        let ev = serde_json::json!({ "data": "no type here" });
        assert!(parse_bridge_event(&ev).is_none());
    }

    #[test]
    fn parse_empty_object_returns_none() {
        let ev = serde_json::json!({});
        assert!(parse_bridge_event(&ev).is_none());
    }
}
