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

/// Normalize an auth token: an empty/whitespace-only `Some("")` collapses to `None`
/// (no auth) with a loud warning (audit B2).
///
/// A `Some("")` token would otherwise enable the auth middleware AND report
/// `auth_required: true` while accepting an empty Bearer credential — an
/// auth-enabled-but-bypassable state. Applied uniformly to both the request gate
/// and the discovery-file token so they can never disagree.
#[must_use]
fn normalize_auth_token(auth_token: Option<String>) -> Option<String> {
    match auth_token {
        Some(t) if t.trim().is_empty() => {
            tracing::warn!(
                "Victauri: configured auth token is empty/whitespace — treating as NO auth. \
                 Set a non-empty VICTAURI_AUTH_TOKEN / auth_token(), or use auth_disabled() \
                 to intentionally run without authentication."
            );
            None
        }
        other => other,
    }
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
///
/// The MCP transport runs **stateless** (the default since the 422 stale-session fix). For the
/// stateful transport (sessions + server-initiated SSE push, required by MCP resource
/// *subscriptions*) use [`build_app_stateful`].
pub fn build_app_full(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    auth_token: Option<String>,
    rate_limiter: Option<Arc<crate::auth::RateLimiterState>>,
) -> axum::Router {
    build_app_full_inner(state, bridge, auth_token, rate_limiter, false)
}

/// Build an Axum router whose MCP transport runs in **stateful** mode (sessions + a long-lived SSE
/// channel), for clients that require the session-based Streamable-HTTP protocol.
///
/// The production default ([`build_app_full`]) is *stateless* because stateful mode mints an
/// in-memory `Mcp-Session-Id` that dies on app restart / idle / SSE drop, after which rmcp answers
/// `422` and generic MCP clients wedge for the whole run. Opt into stateful only if your client
/// needs the session protocol. (Note: Victauri does not currently implement server-initiated
/// resource-update push, so neither transport delivers MCP resource *subscription* notifications;
/// the `subscribe` capability is intentionally not advertised — read resources on demand.)
#[doc(hidden)]
pub fn build_app_stateful(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    auth_token: Option<String>,
) -> axum::Router {
    build_app_full_inner(state, bridge, auth_token, None, true)
}

fn build_app_full_inner(
    state: Arc<VictauriState>,
    bridge: Arc<dyn WebviewBridge>,
    auth_token: Option<String>,
    rate_limiter: Option<Arc<crate::auth::RateLimiterState>>,
    stateful: bool,
) -> axum::Router {
    // Normalize an empty/whitespace-only auth token to "no auth" (audit B2) so the
    // server is never "looks protected, isn't".
    let auth_token = normalize_auth_token(auth_token);

    // Capture the host app's identity for `/info` (first-contact verification: an agent
    // can confirm it reached the RIGHT app, not another Victauri instance on a shared port).
    let tauri_cfg = bridge.tauri_config();
    let app_identifier = tauri_cfg
        .get("identifier")
        .and_then(|v| v.as_str())
        .map(String::from);
    let app_product_name = tauri_cfg
        .get("product_name")
        .and_then(|v| v.as_str())
        .map(String::from);

    let handler = VictauriMcpHandler::new(state.clone(), bridge);
    let rest = super::rest::router(handler.clone());

    // Run the Streamable-HTTP MCP transport STATELESS by default (rmcp's default is stateful).
    //
    // Why: stateful mode mints an in-memory `Mcp-Session-Id` at `initialize` that every later
    // request must echo. That session dies on app restart (the in-memory store is gone — and a
    // `tauri dev` app restarts constantly), on idle eviction, or on SSE-stream drop. rmcp then
    // answers the next call with `422 "expected initialize request"`. The MCP spec signals an
    // expired session with `404` (clients re-init on that); `422` is non-standard, so a generic
    // MCP client (e.g. the agent harness, which speaks rmcp directly and can't use our recovering
    // `victauri bridge`) never recognises it as "re-init needed" and stays wedged for the whole
    // run — the root cause of falling back to the REST API for everything.
    //
    // Stateless mode has no session id and no session to lose, so the 422 class cannot occur. The
    // handler is already built per-request (`move || Ok(handler.clone())` above), exactly what
    // stateless mode needs. `with_json_response(true)` returns `application/json` directly instead
    // of an SSE frame for these request/response tools (the test client and `victauri bridge`
    // already parse JSON-or-SSE, so this is transparent). The only capability given up is
    // server-initiated SSE push — i.e. MCP resource *subscriptions* (`victauri://{ipc-log,windows,
    // state}` notify); all 35 request/response tools and one-shot `resources/read` are unaffected.
    // `build_app_stateful` (`stateful = true`) restores the session/SSE transport for subscribers.
    //
    // NB: `StreamableHttpServerConfig` is `#[non_exhaustive]`, so it cannot be built with struct
    // literal syntax outside rmcp — the builder methods are the only way to override defaults.
    let mcp_config = if stateful {
        StreamableHttpServerConfig::default()
    } else {
        StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true)
    };
    let mcp_service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
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
                let app_id = app_identifier.clone();
                let app_name = app_product_name.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "name": "victauri",
                        "description": "Full-stack Tauri app inspection: webview + IPC + Rust backend + SQLite",
                        "version": env!("CARGO_PKG_VERSION"),
                        "protocol": "mcp",
                        // Host-app identity — lets an agent verify it reached the intended app.
                        "app_identifier": app_id,
                        "app_product_name": app_name,
                        "capabilities": ["webview", "ipc", "backend", "database", "filesystem"],
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
    // Normalize once so the discovery-file token and the request gate agree (B2):
    // an empty token must not be written to the discovery file as if auth were on.
    let auth_token = normalize_auth_token(auth_token);
    let token_for_file = auth_token.clone();
    let app = build_app_with_options(state.clone(), bridge.clone(), auth_token);

    let (listener, actual_port) = try_bind(port).await?;

    if actual_port != port {
        tracing::warn!("Victauri: port {port} in use, fell back to {actual_port}");
    }

    state.port.store(actual_port, Ordering::Relaxed);
    let cfg = bridge.tauri_config();
    let app_identifier = cfg.get("identifier").and_then(|v| v.as_str());
    let app_product_name = cfg.get("product_name").and_then(|v| v.as_str());
    write_port_file(actual_port, app_identifier, app_product_name);
    // Always write a session token to the discovery directory so clients can
    // authenticate automatically.  When auth is explicitly configured the
    // configured token is used; otherwise a fresh UUID is generated.  The auth
    // middleware is only enabled when `auth_token` is `Some`, so this file is
    // purely informational when auth is off — sending the token header is a
    // harmless no-op.
    let discovery_token = token_for_file
        .as_deref()
        .map_or_else(crate::auth::generate_token, String::from);
    write_token_file(&discovery_token);

    tracing::info!("Victauri MCP server listening on 127.0.0.1:{actual_port}");

    let drain_state = state.clone();
    let drain_bridge = bridge;
    let drain_shutdown = state.shutdown_tx.subscribe();
    let drain_finished = state.task_tracker.track("event_drain_loop");
    tokio::spawn(async move {
        event_drain_loop(drain_state, drain_bridge, drain_shutdown).await;
        drain_finished.store(true, std::sync::atomic::Ordering::Relaxed);
    });

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
        // Saturating/checked add: a `preferred` near u16::MAX (e.g. 65530) would
        // otherwise overflow `preferred + offset` (panic in debug, wrap in release).
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        if let Ok(listener) = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await {
            return Ok((listener, port));
        }
    }

    anyhow::bail!(
        "could not bind to any port in range {preferred}-{}",
        preferred.saturating_add(PORT_FALLBACK_RANGE)
    )
}

fn discovery_dir() -> std::path::PathBuf {
    std::env::temp_dir()
        .join("victauri")
        .join(std::process::id().to_string())
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = std::env::temp_dir().join(format!(
            ".victauri_plugin_uidprobe_{}_{}",
            std::process::id(),
            sequence
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .ok();
        if let Some(file) = file {
            let uid = file.metadata().ok().map(|m| m.uid());
            drop(file);
            let _ = std::fs::remove_file(probe);
            if uid.is_some() {
                return uid;
            }
        }
    }
    None
}

#[cfg(unix)]
fn ensure_unix_private_dir(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    let Some(euid) = current_euid() else {
        return false;
    };
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_dir() || meta.uid() != euid {
                return false;
            }
            if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).is_err() {
                return false;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            if builder.create(path).is_err() {
                return false;
            }
        }
        Err(_) => return false,
    }
    unix_private_dir_is_trusted(path)
}

#[cfg(unix)]
fn unix_private_dir_is_trusted(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Some(euid) = current_euid() else {
        return false;
    };
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.file_type().is_dir() && meta.uid() == euid && (meta.permissions().mode() & 0o077) == 0
    })
}

/// Restrict a file or directory to current-user-only access on Windows via `icacls`.
#[cfg(windows)]
#[allow(unsafe_code)]
fn current_windows_username() -> Option<String> {
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    use windows::core::PWSTR;

    let mut buffer = [0_u16; 257];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` is writable for `len` UTF-16 code units and remains alive
    // for the duration of the call. `GetUserNameW` writes at most that capacity.
    unsafe {
        GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &raw mut len).ok()?;
    }
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(len as usize);
    String::from_utf16(&buffer[..end])
        .ok()
        .filter(|name| !name.is_empty())
}

#[cfg(windows)]
fn restrict_to_current_user(path: &std::path::Path) -> bool {
    let Some(username) = current_windows_username() else {
        return false;
    };
    let path_str = path.to_string_lossy();
    // `/inheritance:r` strips INHERITED ACEs and `/grant:r USER:F` replaces only USER's grant —
    // neither removes a pre-planted EXPLICIT ACE for another principal (e.g. an attacker who
    // guessed our PID on a shared TEMP and pre-created the dir with `Everyone:(F)`). Explicitly
    // remove the common world/group principals (Everyone S-1-1-0, BUILTIN\Users S-1-5-32-545,
    // Authenticated Users S-1-5-11) before granting owner-only. (Residual: a custom-SID pre-plant
    // on a non-default shared TEMP still survives; the Windows default per-user TEMP closes that.)
    std::process::Command::new("icacls")
        .args([
            &*path_str,
            "/inheritance:r",
            "/remove",
            "*S-1-1-0",
            "*S-1-5-32-545",
            "*S-1-5-11",
            "/grant:r",
            &format!("{username}:F"),
            "/q",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Trust the discovery path only when both the shared root and PID directory are owned
/// by this process's effective user. Refuse planted paths instead of deleting them.
fn ensure_private_dir(dir: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        let Some(root) = dir.parent() else {
            return false;
        };
        if !ensure_unix_private_dir(root) || !ensure_unix_private_dir(dir) {
            tracing::warn!("refusing untrusted discovery path {}", dir.display());
            return false;
        }
    }
    #[cfg(not(unix))]
    {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        #[cfg(windows)]
        if !restrict_to_current_user(dir) {
            let _ = std::fs::remove_dir_all(dir);
            return false;
        }
    }
    true
}

/// Write `contents` to `path` as a fresh, user-only file. Uses exclusive
/// (`create_new` / `O_EXCL`) creation so a pre-planted file OR symlink at `path`
/// is refused rather than written through, and sets `0600` at creation on Unix so
/// there is no window where the file exists with default-umask permissions.
fn write_private_file(path: &std::path::Path, contents: &str) {
    // Clear any stale/pre-planted entry (symlink-aware) so our exclusive create
    // succeeds for a fresh file; a symlink racing in afterwards is refused by
    // `create_new` (O_EXCL treats a final-component symlink as "exists").
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(path);
    }
    #[cfg(unix)]
    let result = {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| f.write_all(contents.as_bytes()))
    };
    #[cfg(not(unix))]
    let result = {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .and_then(|mut f| f.write_all(contents.as_bytes()))
    };
    // Report a write failure; on Windows additionally lock the new file down to the
    // current user and remove it if the ACL cannot be applied (never leave a discovery
    // file world-readable). Split per-platform so neither config trips `-D warnings`:
    // the Windows-only post-step would otherwise make an early `return` needless on Unix.
    #[cfg(windows)]
    match result {
        Ok(()) => {
            if !restrict_to_current_user(path) {
                let _ = std::fs::remove_file(path);
                tracing::warn!("could not restrict discovery file {}", path.display());
            }
        }
        Err(e) => {
            tracing::debug!("could not write discovery file {}: {e}", path.display());
        }
    }
    #[cfg(not(windows))]
    if let Err(e) = result {
        tracing::debug!("could not write discovery file {}: {e}", path.display());
    }
}

fn write_port_file(port: u16, identifier: Option<&str>, product_name: Option<&str>) {
    let dir = discovery_dir();
    if !ensure_private_dir(&dir) {
        return;
    }
    write_private_file(&dir.join("port"), &port.to_string());
    // Write metadata for multi-server discovery. The app `identifier` lets a discovery
    // client (e.g. `victauri bridge --app <id>`) select the RIGHT app when several Victauri
    // instances are running, instead of guessing — the root cause of agents binding to the
    // wrong process on a shared port.
    let metadata = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "identifier": identifier,
        "product_name": product_name,
        "started_at": chrono::Utc::now().to_rfc3339(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    write_private_file(&dir.join("metadata.json"), &metadata.to_string());
}

fn write_token_file(token: &str) {
    let dir = discovery_dir();
    if !ensure_private_dir(&dir) {
        return;
    }
    write_private_file(&dir.join("token"), token);
}

fn remove_port_file() {
    let dir = discovery_dir();
    #[cfg(unix)]
    {
        let Some(root) = dir.parent() else {
            return;
        };
        if !unix_private_dir_is_trusted(root) || !unix_private_dir_is_trusted(&dir) {
            return;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
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
        "console" => AppEvent::Console {
            level: ev
                .get("level")
                .and_then(|l| l.as_str())
                .unwrap_or("log")
                .to_string(),
            message: ev
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            timestamp: now,
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

        let id_js = super::helpers::js_string(&id);
        let inject = format!(
            r"
            (async () => {{
                try {{
                    const __result = await (async () => {{ {code} }})();
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
                        result: JSON.stringify(__result)
                    }});
                }} catch (e) {{
                    await window.__TAURI_INTERNALS__.invoke('plugin:victauri|victauri_eval_callback', {{
                        id: {id_js},
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
                state.event_log.push(app_event.clone());
                if state.recorder.is_recording() {
                    state.recorder.record_event(app_event);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use victauri_core::{AppEvent, InteractionKind, IpcResult};

    #[test]
    fn normalize_auth_token_collapses_empty() {
        // Audit B2: an empty/whitespace token must become "no auth", never an
        // auth-enabled-but-empty-credential state.
        assert_eq!(normalize_auth_token(Some(String::new())), None);
        assert_eq!(normalize_auth_token(Some("   ".to_string())), None);
        assert_eq!(normalize_auth_token(Some("\t\n".to_string())), None);
        // A real token is preserved; explicit None stays None.
        assert_eq!(
            normalize_auth_token(Some("secret-123".to_string())).as_deref(),
            Some("secret-123")
        );
        assert_eq!(normalize_auth_token(None), None);
    }

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
        write_port_file(7777, Some("com.example.app"), Some("Example"));
        let dir = discovery_dir();
        let content = std::fs::read_to_string(dir.join("port")).unwrap();
        assert_eq!(content, "7777");
        // Metadata file written
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("metadata.json")).unwrap())
                .unwrap();
        assert_eq!(meta["port"], 7777);
        assert_eq!(meta["pid"], std::process::id());
        // App identity must be recorded so a discovery client can select the RIGHT app.
        assert_eq!(meta["identifier"], "com.example.app");
        assert_eq!(meta["product_name"], "Example");
        remove_port_file();
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_refuses_symlink_without_chmodding_target() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().unwrap();
        let target = base.path().join("target");
        let link = base.path().join("link");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(!ensure_unix_private_dir(&link));
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "symlink target permissions must be untouched");
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
            AppEvent::Console { level, message, .. } => {
                assert_eq!(level, "warn");
                assert_eq!(message, "deprecated API usage");
            }
            other => panic!("expected Console, got {other:?}"),
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
            AppEvent::Console { level, message, .. } => {
                assert_eq!(level, "log");
                assert_eq!(message, "hello");
            }
            other => panic!("expected Console, got {other:?}"),
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
