//! Stdio-to-HTTP bridge for MCP clients like Claude Code.
//!
//! Reads JSON-RPC messages from stdin, forwards them to Victauri's Streamable HTTP
//! endpoint, parses SSE responses, and writes them back to stdout. This bridges
//! the gap between MCP hosts that expect stdio transport and Victauri's HTTP server.
//!
//! Why this exists (and why agents should connect through it, not a fixed `url:`):
//!
//! * **Connects instantly, app or no app (the cold-start guarantee).** Victauri's MCP
//!   server is embedded *inside* the Tauri app, so it only exists while the app runs. A
//!   naive proxy that discovered the backend before answering `initialize` would hang the
//!   MCP handshake whenever the app was not yet running — and the host (Claude Code) aborts
//!   at a 30s connection timeout, so **every fresh terminal opened before the app started
//!   failed to connect**. This bridge answers `initialize` and `tools/list` **locally**, so
//!   the MCP server always appears connected with its full tool surface. Only actual tool
//!   *calls* need a live backend; when none is running they return a clear, actionable error
//!   instead of hanging. When the app comes up (even minutes later, even after a restart)
//!   the bridge discovers it and emits `notifications/tools/list_changed`, so tools go live
//!   automatically with **no `/mcp` reconnect**.
//! * **Always reaches the RIGHT app.** A static `.mcp.json` URL hardcodes a port; when
//!   several Victauri apps run (or one falls back off a busy 7373), that port can point at
//!   the WRONG process. The bridge resolves the live backend **by app identity** at connect
//!   time and re-resolves on failure — so the agent can never get stuck talking to the
//!   wrong app. Select with `--app <identifier>` (or `VICTAURI_APP`); with no selector it
//!   uses the single running app, or errors clearly if several are running.
//! * **Survives server restarts.** Every dev rebuild/relaunch invalidates the MCP session.
//!   The bridge re-establishes a fresh backend session (re-discovering the port) on a stale
//!   session (404/409/422) or connection drop, so the agent's tool calls keep working
//!   without a reconnect.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::{Value, json};

const MAX_RETRIES: usize = 4;
const RETRY_DELAY_MS: u64 = 400;
/// How often the background availability poller re-checks whether a backend became reachable
/// WHILE DOWN. On a down→up transition it emits `tools/list_changed` so the client swaps the
/// baked fallback tool list for the live one with no reconnect.
const POLL_INTERVAL_MS: u64 = 1500;
/// Slower poll cadence once the backend is already UP — we only need to catch a later restart,
/// so there is no reason to spawn a `tasklist`/`ps` + `/health` probe every 1.5s for the whole
/// session (over an 8h editor session that would be ~19k needless subprocess spawns).
const POLL_INTERVAL_UP_MS: u64 = 5000;
/// MCP protocol version advertised in the local `initialize` reply when the client did not
/// request one. When the client DOES request a version we echo it (guaranteed-accepted).
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
/// Baked-in tool list (name + description per tool), shown while no app is running so the
/// MCP server always presents its full surface. Superseded by the live `tools/list` the
/// moment an app connects. Generated from the plugin's `#[tool]` annotations.
static TOOLS_FALLBACK_JSON: &str = include_str!("tools_fallback.json");
/// Server `instructions` returned by the local `initialize`, so an agent understands the
/// down-vs-up states before it makes a call.
const LOCAL_INIT_INSTRUCTIONS: &str = "Victauri MCP bridge. Tools act on a running Tauri app \
    (debug build). While no app is running the tool list is a static fallback and tool calls \
    report the backend as unreachable; start the app and the bridge connects automatically — \
    the tool list refreshes to the live set with no reconnect.";

/// A discovered, live Victauri backend.
#[derive(Clone, Debug)]
struct ServerInfo {
    port: u16,
    token: Option<String>,
    identifier: Option<String>,
    product_name: Option<String>,
}

impl ServerInfo {
    fn label(&self) -> String {
        let name = self
            .identifier
            .as_deref()
            .or(self.product_name.as_deref())
            .unwrap_or("<unknown app>");
        format!("{name} (port {})", self.port)
    }
}

/// Run the stdio bridge for MCP clients.
///
/// Unlike a naive proxy, this NEVER blocks the MCP handshake on discovering a backend:
/// `initialize`/`tools/list` are answered locally so the server always appears connected,
/// and only tool *calls* require a live app. `app` selects which app to bind when several
/// are running (matches the Tauri bundle identifier or product name; falls back to the
/// `VICTAURI_APP` env var).
///
/// # Errors
///
/// Returns an error only if the HTTP client or stdio pipes cannot be set up — never merely
/// because no app is running.
pub async fn run(wait: bool, app: Option<String>) -> Result<()> {
    // `--wait` is retained for backward compatibility with existing `.mcp.json` files but is
    // now a no-op: the handshake never blocks on discovery, so there is nothing to wait for.
    // Tool calls discover lazily and fail fast with an actionable message when the app is down.
    let _ = wait;
    let app = app.or_else(|| std::env::var("VICTAURI_APP").ok());
    let http = build_client()?;

    // The live backend, discovered lazily. `None` until an app is found — the bridge starts
    // and serves the handshake with no backend at all.
    let connection: Arc<Mutex<Option<ServerInfo>>> = Arc::new(Mutex::new(None));
    let session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // The client's `initialize` message, cached so we can hand-shake the BACKEND (replay it)
    // when we first forward a real request and after a restart. The client is answered
    // locally, so it only ever sends `initialize` once.
    let cached_init: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    // Set once a backend handshake returns no `Mcp-Session-Id`: the server is stateless, so
    // there is no session to mint or lose and we must not re-`initialize` before every call.
    let stateless: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    // Last-known backend availability — SOLELY owned by the poller (see H1 in the audit): the
    // request path never writes it, so the down→up edge is detected in exactly one place.
    let backend_up = Arc::new(AtomicBool::new(false));
    // Set once the client sends `notifications/initialized` (its OWN handshake completion), so
    // the poller never emits a server notification before the client has finished initializing —
    // not merely before we answered `initialize` (audit #3). Gating on the client's ack, not on
    // our local reply, closes the window where `list_changed` could precede the init response.
    let client_ready = Arc::new(AtomicBool::new(false));
    // stdout is shared with the background poller (which writes `list_changed`); every write
    // locks, emits one line, and flushes, so responses and notifications never interleave.
    let stdout = Arc::new(Mutex::new(std::io::stdout()));

    spawn_availability_poller(
        app.clone(),
        Arc::clone(&connection),
        Arc::clone(&session_id),
        Arc::clone(&stateless),
        Arc::clone(&backend_up),
        Arc::clone(&client_ready),
        Arc::clone(&stdout),
    );

    // Read stdin on a DEDICATED OS thread feeding an async channel, so the blocking read never
    // parks a tokio worker (which, on a single-vCPU host, would starve the timer and stop the
    // availability poller from ever firing — H2 in the audit). The async loop stays responsive.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break; // async side gone
            }
        }
        // EOF (or send failure) drops line_tx → the async loop's recv() returns None → shutdown.
    });

    while let Some(line) = line_rx.recv().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("victauri-bridge: invalid JSON on stdin: {e}");
                continue;
            }
        };

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = msg.get("id").is_none();

        match method {
            // ── Answered locally — never block on a backend ──────────────────────
            "initialize" => {
                // Cache for the lazy backend handshake, then reply immediately.
                *locked(&cached_init) = Some(msg.clone());
                write_value(&stdout, &local_initialize_response(&msg));
            }
            // The client's ack of OUR local initialize. It must never reach a backend (we
            // handshake the backend separately) and needs no response — but it is the point at
            // which the client is ready to receive server notifications, so mark it here (NOT on
            // `initialize`) so the poller can't emit `list_changed` before the client is ready.
            "notifications/initialized" => {
                client_ready.store(true, Ordering::Release);
            }
            "ping" => {
                write_value(&stdout, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            // ── List methods: live when up, graceful placeholder when down ───────
            "tools/list" => {
                match forward_when_up(
                    &http,
                    &connection,
                    &session_id,
                    &stateless,
                    &cached_init,
                    app.as_deref(),
                    &msg,
                )
                .await
                {
                    Some(payloads) => write_payloads(&stdout, &payloads),
                    // App down (or a transient fetch miss): serve the full baked fallback so
                    // the agent still sees every tool. The live list arrives via
                    // `tools/list_changed` when the app comes up.
                    None => write_value(&stdout, &fallback_tools_response(&id)),
                }
            }
            "resources/list" | "resources/templates/list" | "prompts/list" => {
                match forward_when_up(
                    &http,
                    &connection,
                    &session_id,
                    &stateless,
                    &cached_init,
                    app.as_deref(),
                    &msg,
                )
                .await
                {
                    Some(payloads) => write_payloads(&stdout, &payloads),
                    // Empty (but valid) result when down — avoids a startup error for a
                    // capability we advertise; the real list arrives via `list_changed`.
                    None => write_value(&stdout, &empty_list_response(method, &id)),
                }
            }
            // ── Everything else (tools/call, resources/read, …) needs a live app ─
            _ => {
                match forward_with_retries(
                    &http,
                    &connection,
                    &session_id,
                    &stateless,
                    &cached_init,
                    app.as_deref(),
                    &msg,
                )
                .await
                {
                    ForwardResult::Payloads(payloads) => write_payloads(&stdout, &payloads),
                    ForwardResult::Accepted => {}
                    ForwardResult::Unreachable(err_msg) => {
                        // A notification to a down backend is simply dropped (no id to answer).
                        if !is_notification {
                            write_value(
                                &stdout,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": { "code": -32000, "message": err_msg }
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// The `initialize` reply the bridge synthesizes itself, so the MCP server is "connected"
/// the instant Claude Code launches — with no app running. Advertises `tools`/`resources`
/// with `listChanged` so the client refreshes its lists when the app later comes up.
fn local_initialize_response(client_msg: &Value) -> Value {
    let id = client_msg.get("id").cloned().unwrap_or(Value::Null);
    // Echo the client's requested protocol version (guaranteed-acceptable to it); fall back
    // to a known version only if it sent none.
    let protocol_version = client_msg
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
        .to_string();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol_version,
            // Mirror the plugin's real capabilities: it advertises `resources` (read) with
            // list-change notifications but DELIBERATELY not `subscribe` — server-initiated
            // resource-update push is not implemented, and this proxy has no channel to deliver
            // it. Advertising `subscribe` here would mislead a client into waiting forever.
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": { "listChanged": true }
            },
            "serverInfo": { "name": "victauri-bridge", "version": env!("CARGO_PKG_VERSION") },
            "instructions": LOCAL_INIT_INSTRUCTIONS
        }
    })
}

/// The full baked tool list as MCP `Tool` objects. Each carries the real name + description
/// and a permissive object input schema — enough for an agent to see what exists while the
/// app is down; the live `tools/list` (with exact per-tool schemas) supersedes it on connect.
fn fallback_tools() -> Vec<Value> {
    let parsed: Vec<Value> = serde_json::from_str(TOOLS_FALLBACK_JSON).unwrap_or_default();
    parsed
        .into_iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_string();
            Some(json!({
                "name": name,
                "description": description,
                "inputSchema": { "type": "object" }
            }))
        })
        .collect()
}

fn fallback_tools_response(id: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": fallback_tools() } })
}

/// A valid but empty list result for a `*/list` method served while the app is down.
fn empty_list_response(method: &str, id: &Value) -> Value {
    let key = match method {
        "resources/list" => "resources",
        "resources/templates/list" => "resourceTemplates",
        "prompts/list" => "prompts",
        _ => "items",
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": { key: [] } })
}

/// Lock a mutex, recovering the guard even if a previous holder panicked (poison), rather than
/// cascading a single panic into the death of the whole bridge (or, silently, the reconnect
/// poller). Mirrors the poison-tolerant pattern used across `victauri-core`.
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write one JSON-RPC value as a line to the shared stdout.
fn write_value(stdout: &Arc<Mutex<std::io::Stdout>>, v: &Value) {
    let mut o = locked(stdout);
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// Relay already-serialized JSON-RPC payload lines (from a backend response) verbatim.
fn write_payloads(stdout: &Arc<Mutex<std::io::Stdout>>, payloads: &[String]) {
    let mut o = locked(stdout);
    for payload in payloads {
        let _ = writeln!(o, "{payload}");
    }
    let _ = o.flush();
}

/// Emit a server→client JSON-RPC notification (no id).
fn write_notification(stdout: &Arc<Mutex<std::io::Stdout>>, method: &str) {
    write_value(stdout, &json!({ "jsonrpc": "2.0", "method": method }));
}

/// Build one JSON-RPC error-response line for `msg`'s id — used when a forwarded REQUEST gets
/// no relayable payload (an empty/non-JSON 2xx body, or a 202), so the client's id never hangs.
fn error_for_request(msg: &Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": msg.get("id"),
        "error": { "code": code, "message": message }
    })
    .to_string()
}

/// Actionable message returned when a tool call can't reach a backend.
fn unreachable_message() -> String {
    "Victauri backend not reachable: no running Tauri app with the Victauri plugin (debug \
     build) was found. Start the app (e.g. `npm run tauri dev` / `pnpm tauri dev`); the bridge \
     connects automatically when it comes up — no reconnect needed. If several Victauri apps \
     run, select one with `--app <bundle-identifier>` or the VICTAURI_APP env var."
        .to_string()
}

/// Background task that watches for the backend becoming reachable and, on a down→up
/// transition, tells the client to refresh its tool/resource lists — so the baked fallback
/// is replaced by the live, version-accurate set with no `/mcp` reconnect.
#[allow(clippy::too_many_arguments)]
fn spawn_availability_poller(
    app: Option<String>,
    connection: Arc<Mutex<Option<ServerInfo>>>,
    session_id: Arc<Mutex<Option<String>>>,
    stateless: Arc<Mutex<bool>>,
    backend_up: Arc<AtomicBool>,
    client_ready: Arc<AtomicBool>,
    stdout: Arc<Mutex<std::io::Stdout>>,
) {
    tokio::spawn(async move {
        loop {
            // Poll fast while DOWN (a freshly-started app is noticed within ~1.5s) and back off
            // while UP (we only need to catch a later restart).
            let interval = if backend_up.load(Ordering::Acquire) {
                POLL_INTERVAL_UP_MS
            } else {
                POLL_INTERVAL_MS
            };
            tokio::time::sleep(Duration::from_millis(interval)).await;
            let found = discover_one(app.as_deref()).await;
            let up = found.is_some();
            if let Some(info) = found {
                *locked(&connection) = Some(info);
            } else {
                // Backend gone — drop the cached connection/session so nothing can reuse a stale
                // (port, token) pair (defense-in-depth for audit #1; the request path also
                // re-resolves the trusted entry on every call).
                *locked(&connection) = None;
                *locked(&session_id) = None;
                *locked(&stateless) = false;
            }
            // The poller is the SOLE owner of `backend_up`: the request path no longer writes it,
            // so the down→up edge is detected here exactly once and can never be silently consumed
            // by a tool call that happened to reconnect first.
            let was = backend_up.swap(up, Ordering::AcqRel);
            // Only announce a refresh once the CLIENT has finished initializing (sent
            // `notifications/initialized`) — a server notification before that is a lifecycle
            // violation a strict client may reject.
            if up && !was && client_ready.load(Ordering::Acquire) {
                write_notification(&stdout, "notifications/tools/list_changed");
                write_notification(&stdout, "notifications/resources/list_changed");
            }
        }
    });
}

/// Outcome of forwarding one JSON-RPC message to a live backend.
enum ForwardResult {
    /// Backend responded — relay these serialized payload lines to the client.
    Payloads(Vec<String>),
    /// A notification the backend accepted (202) — nothing to relay.
    Accepted,
    /// No live backend could be reached — the message carries an actionable explanation.
    Unreachable(String),
}

/// Forward a list-style request, returning its payloads only if a backend is currently
/// reachable; `None` when the app is down (the caller then serves a local placeholder). Delegates
/// to `forward_with_retries`, which re-resolves the trusted backend itself — so a live `tools/list`
/// pays exactly one discovery pass and a down one fails fast to the fallback.
#[allow(clippy::too_many_arguments)]
async fn forward_when_up(
    http: &reqwest::Client,
    connection: &Arc<Mutex<Option<ServerInfo>>>,
    session_id: &Arc<Mutex<Option<String>>>,
    stateless: &Arc<Mutex<bool>>,
    cached_init: &Arc<Mutex<Option<Value>>>,
    app: Option<&str>,
    msg: &Value,
) -> Option<Vec<String>> {
    match forward_with_retries(
        http,
        connection,
        session_id,
        stateless,
        cached_init,
        app,
        msg,
    )
    .await
    {
        ForwardResult::Payloads(payloads) => Some(payloads),
        _ => None,
    }
}

/// Forward a message to the live backend, establishing/recovering the backend session and
/// retrying across a restart. Discovers a backend first if we don't have one; returns
/// `Unreachable` (never blocks indefinitely) when no app is running.
async fn forward_with_retries(
    http: &reqwest::Client,
    connection: &Arc<Mutex<Option<ServerInfo>>>,
    session_id: &Arc<Mutex<Option<String>>>,
    stateless: &Arc<Mutex<bool>>,
    cached_init: &Arc<Mutex<Option<Value>>>,
    app: Option<&str>,
    msg: &Value,
) -> ForwardResult {
    let is_notification = msg.get("id").is_none();

    // SECURITY (audit #1): re-resolve the trusted backend on EVERY forward — never reuse a cached
    // `(port, token)` without re-confirming, right now, that the port still belongs to a live,
    // trusted, identity-matched app. `scan_once` re-applies `dir_is_trusted` + liveness + `--app`
    // identity and yields the port and token together. Without this, after the app shut down (its
    // discovery entry gone) an attacker who bound the freed port would receive the cached Bearer
    // token and could relay forged tool results. This is ONE discovery pass (no 1s-sleep retries):
    // a live app resolves fast; a down app fails fast to the actionable message / fallback.
    match scan_once(app).await {
        Selection::One(info) => {
            *locked(connection) = Some(info);
        }
        Selection::Ambiguous(labels) => {
            *locked(connection) = None;
            return ForwardResult::Unreachable(format!(
                "Multiple Victauri apps are running:\n  {}\nSelect one with \
                 `--app <bundle-identifier>` or the VICTAURI_APP env var.",
                labels.join("\n  ")
            ));
        }
        Selection::None => {
            *locked(connection) = None;
            return ForwardResult::Unreachable(unreachable_message());
        }
    }

    let mut last_err = None;

    for attempt in 0..MAX_RETRIES {
        // Re-establish a fresh backend session BEFORE replaying the real request when we have
        // none (first forward, or after a restart invalidated it). This is what makes
        // restart-recovery work — replaying a tool call with no session would 422.
        if !*locked(stateless) {
            let need_reinit = locked(session_id).is_none();
            if need_reinit {
                let init = locked(cached_init).clone();
                if let Some(init) = init
                    && let Some((port, token)) = conn_parts(connection)
                {
                    // We don't relay the backend handshake response to the client; it already
                    // believes it is initialized (we answered locally).
                    if let Ok(out) = post_message(http, port, token.as_deref(), None, &init).await {
                        let backend_sid = out.session_id.clone();
                        if let Some(sid) = out.session_id {
                            *locked(session_id) = Some(sid);
                        } else if !out.stale_session {
                            // Handshake succeeded with no session id → stateless backend.
                            *locked(stateless) = true;
                        }
                        // Complete the MCP lifecycle for a STATEFUL backend (harmless for the
                        // stateless default): the client's `notifications/initialized` was
                        // answered locally, so replay it to the backend now — a stateful rmcp
                        // session may gate tool calls on having received it.
                        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
                        let _ = post_message(
                            http,
                            port,
                            token.as_deref(),
                            backend_sid.as_deref(),
                            &note,
                        )
                        .await;
                    }
                }
            }
        }

        let Some((port, token)) = conn_parts(connection) else {
            return ForwardResult::Unreachable(unreachable_message());
        };
        let sid = locked(session_id).clone();

        match post_message(http, port, token.as_deref(), sid.as_deref(), msg).await {
            Ok(out) => {
                if let Some(new_sid) = out.session_id {
                    *locked(session_id) = Some(new_sid);
                }

                if out.stale_session {
                    eprintln!(
                        "victauri-bridge: stale session (HTTP {}), re-establishing (attempt {}/{})",
                        out.status,
                        attempt + 1,
                        MAX_RETRIES
                    );
                    *locked(session_id) = None;
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                        if let Ok(new_conn) = discover_and_select(false, app).await {
                            *locked(connection) = Some(new_conn);
                        }
                    }
                    last_err = Some(format!("Victauri returned {}", out.status));
                    continue;
                }

                if out.accepted {
                    if is_notification {
                        return ForwardResult::Accepted;
                    }
                    // A 202 to a REQUEST leaves no body to relay — Victauri never does this, but a
                    // proxy must not leave the client's id hanging if a backend ever does.
                    return ForwardResult::Payloads(vec![error_for_request(
                        msg,
                        -32603,
                        "backend accepted the request with no response (HTTP 202)",
                    )]);
                }

                // A successful response that yields NO relayable payload for a REQUEST (an empty
                // or non-JSON 2xx body — see `post_message`) must likewise not hang the client's
                // id: synthesize an error rather than returning empty payloads (audit #2).
                if !is_notification && out.payloads.is_empty() {
                    return ForwardResult::Payloads(vec![error_for_request(
                        msg,
                        -32603,
                        "backend returned an empty or non-JSON response",
                    )]);
                }

                return ForwardResult::Payloads(out.payloads);
            }
            Err(e) => {
                eprintln!(
                    "victauri-bridge: connection failed (attempt {}/{}): {e}",
                    attempt + 1,
                    MAX_RETRIES
                );
                *locked(session_id) = None;
                if attempt + 1 < MAX_RETRIES {
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_DELAY_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    // Re-discover; the app may have restarted on a new port, or gone away.
                    match discover_and_select(false, app).await {
                        Ok(new_conn) => {
                            eprintln!("victauri-bridge: reconnected to {}", new_conn.label());
                            *locked(connection) = Some(new_conn);
                        }
                        Err(_) => {
                            *locked(connection) = None;
                        }
                    }
                }
                last_err = Some(format!("Victauri server unreachable ({e})"));
                continue;
            }
        }
    }

    // Retries exhausted — the app is down or perpetually restarting. `last_err` is logged to
    // stderr already; the client gets the actionable remedy.
    let _ = last_err;
    ForwardResult::Unreachable(unreachable_message())
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(Into::into)
}

fn conn_parts(connection: &Arc<Mutex<Option<ServerInfo>>>) -> Option<(u16, Option<String>)> {
    locked(connection)
        .as_ref()
        .map(|s| (s.port, s.token.clone()))
}

/// Outcome of forwarding one JSON-RPC message to the backend.
struct PostOutcome {
    status: u16,
    session_id: Option<String>,
    stale_session: bool,
    accepted: bool,
    payloads: Vec<String>,
}

/// Forward a single JSON-RPC message to `127.0.0.1:<port>/mcp` and parse the response.
async fn post_message(
    http: &reqwest::Client,
    port: u16,
    token: Option<&str>,
    session_id: Option<&str>,
    msg: &serde_json::Value,
) -> Result<PostOutcome> {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let mut req = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(sid) = session_id {
        req = req.header("Mcp-Session-Id", sid);
    }

    let resp = req.json(msg).send().await?;
    let status = resp.status().as_u16();
    let new_sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // 404/409 = unknown/terminated session; 422 = "expect initialize" (no/!init session).
    // All three mean "the session is gone — re-establish it".
    let stale_session = matches!(status, 404 | 409 | 422);
    let accepted = status == 202;

    let mut payloads = Vec::new();
    if !stale_session && status != 202 {
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await.unwrap_or_default();

        if !(200..300).contains(&status) {
            // Surface a JSON-RPC error for the original request id.
            payloads.push(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": msg.get("id"),
                    "error": { "code": -32000, "message": format!("Victauri returned {status}: {body}") }
                })
                .to_string(),
            );
        } else if content_type.contains("text/event-stream") {
            for sse_line in body.lines() {
                if let Some(data) = sse_line.strip_prefix("data: ") {
                    let data = data.trim();
                    if !data.is_empty() && serde_json::from_str::<serde_json::Value>(data).is_ok() {
                        payloads.push(data.to_string());
                    }
                }
            }
        } else {
            let body = body.trim();
            // Relay a non-SSE body ONLY if it is valid JSON — never write non-JSON onto the
            // client's JSON-RPC stream. An empty or non-JSON 2xx body yields no payload; the
            // caller then synthesizes an error for a request so the client's id can't hang.
            if !body.is_empty() && serde_json::from_str::<serde_json::Value>(body).is_ok() {
                payloads.push(body.to_string());
            }
        }
    }

    Ok(PostOutcome {
        status,
        session_id: new_sid,
        stale_session,
        accepted,
        payloads,
    })
}

/// One discovery pass: honor a `VICTAURI_PORT` override, else scan the discovery dir for
/// live, health-checked backends and select the one matching `app` (or the sole one). Does
/// not sleep or retry — callers add patience if they want it.
async fn scan_once(app: Option<&str>) -> Selection {
    // Explicit env override wins (a developer pinning a specific port).
    if let Ok(p) = std::env::var("VICTAURI_PORT")
        && let Ok(port) = p.parse::<u16>()
        && health_ok(port).await
    {
        return Selection::One(ServerInfo {
            port,
            // An EMPTY/whitespace `VICTAURI_AUTH_TOKEN` is "not configured", NOT "send an
            // empty Bearer" — it must fall through to the discovered token for this exact
            // port (see `normalize_env_token`).
            token: normalize_env_token(std::env::var("VICTAURI_AUTH_TOKEN").ok())
                .or_else(|| discover_token_for_port(port)),
            identifier: None,
            product_name: None,
        });
    }

    // Liveness-FIRST (batched), then health. `alive_pids` snapshots every live PID in ONE OS
    // call (~60ms), so a machine with many stale discovery dirs (dead dev sessions) is filtered
    // for ~free instead of one process spawn PER dir. Crucially we then health-check ONLY the
    // live-pid entries: a stale dir whose long-dead port was reused by some other service is
    // never probed (that mis-order made a down-state scan hang on an unresponsive reused port).
    // When the app is down there are zero live entries, so zero health probes — the poller
    // stays cheap. `None` (enumeration unavailable) falls back to the per-pid check.
    let alive = alive_pids();
    let is_alive = |pid: u32| {
        alive
            .as_ref()
            .map_or_else(|| is_process_alive(pid), |set| set.contains(&pid))
    };
    let mut live = Vec::new();
    for (pid, s) in discover_entries() {
        if is_alive(pid) && health_ok(s.port).await {
            live.push(s);
        }
    }
    select(&live, app)
}

/// A single non-blocking discovery attempt — `Some` iff exactly one matching live backend is
/// found. Used by the availability poller and the `*/list` fast path.
async fn discover_one(app: Option<&str>) -> Option<ServerInfo> {
    match scan_once(app).await {
        Selection::One(s) => Some(s),
        _ => None,
    }
}

/// Discover live Victauri backends and select the one matching `app` (or the only one),
/// retrying for a short window (or ~30s under `wait`).
async fn discover_and_select(wait: bool, app: Option<&str>) -> Result<ServerInfo> {
    let max_attempts = if wait { 30 } else { 3 };
    let delay = Duration::from_secs(1);

    for attempt in 0..max_attempts {
        match scan_once(app).await {
            Selection::One(s) => {
                eprintln!("victauri-bridge: connected to {}", s.label());
                return Ok(s);
            }
            Selection::None if attempt + 1 < max_attempts => {
                if attempt == 0 {
                    eprintln!("victauri-bridge: waiting for Victauri server...");
                }
                tokio::time::sleep(delay).await;
            }
            Selection::None => {
                bail!(
                    "Could not connect to Victauri server.\n\
                     Is your Tauri app running (debug build)? Start it with: pnpm run tauri dev"
                );
            }
            Selection::Ambiguous(labels) => {
                bail!(
                    "Multiple Victauri apps are running:\n  {}\n\
                     Specify which one with `victauri bridge --app <identifier>` (or set \
                     VICTAURI_APP). The identifier is your Tauri bundle identifier.",
                    labels.join("\n  ")
                );
            }
        }
    }

    bail!("Could not connect to a matching Victauri server")
}

enum Selection {
    One(ServerInfo),
    None,
    Ambiguous(Vec<String>),
}

/// Pick the server matching `app`, or the sole running server.
fn select(live: &[ServerInfo], app: Option<&str>) -> Selection {
    if live.is_empty() {
        return Selection::None;
    }
    if let Some(app) = app {
        let needle = app.to_ascii_lowercase();
        // Prefer an exact identifier/product_name match, then a substring match.
        let exact = live.iter().find(|s| {
            s.identifier
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
                == Some(&needle)
                || s.product_name
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                    == Some(&needle)
        });
        if let Some(s) = exact {
            return Selection::One(s.clone());
        }
        let partial = live.iter().find(|s| {
            s.identifier
                .as_deref()
                .is_some_and(|i| i.to_ascii_lowercase().contains(&needle))
                || s.product_name
                    .as_deref()
                    .is_some_and(|p| p.to_ascii_lowercase().contains(&needle))
        });
        return match partial {
            Some(s) => Selection::One(s.clone()),
            None => Selection::None,
        };
    }
    // No app specified: fine if exactly one is running; ambiguous otherwise.
    if live.len() == 1 {
        Selection::One(live[0].clone())
    } else {
        Selection::Ambiguous(live.iter().map(ServerInfo::label).collect())
    }
}

/// Read `<temp>/victauri/<pid>/` discovery entries (port + token + identity) as
/// `(pid, ServerInfo)`. Pure filesystem work — NO process-liveness or HTTP health check here,
/// so it stays cheap even with many stale directories. Callers apply the health-then-liveness
/// filter in [`scan_once`], which is what keeps a down-state scan fast: a dead/stale port
/// refuses the connection instantly, so no process enumeration runs at all.
fn discover_entries() -> Vec<(u32, ServerInfo)> {
    let root = std::env::temp_dir().join("victauri");
    let mut out = Vec::new();
    // The root itself is security-sensitive: its owner can rename a trusted PID
    // directory after our child check and swap in attacker-controlled files.
    if !dir_is_trusted(&root) {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let pid_str = entry.file_name().to_string_lossy().to_string();
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let dir = entry.path();
        // Shared-temp hardening (audit #15, read side). The discovery root lives under a
        // world-writable temp dir on Unix, so a local attacker can plant a fake `<pid>`
        // directory — named after one of THEIR own live processes, so the liveness check
        // passes — pointing at a server they control, and harvest the real Bearer token we
        // send it (and feed us forged tool results). Trust a directory only if it is a real
        // directory we own and is not group/other-writable — the same guard
        // `victauri-test::discovery` already applies. The bridge is the path Claude Code
        // connects through, so this is the highest-value read-side sink.
        if !dir_is_trusted(&dir) {
            continue;
        }
        let Ok(port_s) = std::fs::read_to_string(dir.join("port")) else {
            continue;
        };
        let Ok(port) = port_s.trim().parse::<u16>() else {
            continue;
        };
        let token = std::fs::read_to_string(dir.join("token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let (identifier, product_name) = std::fs::read_to_string(dir.join("metadata.json"))
            .ok()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .map_or((None, None), |m| {
                (
                    m.get("identifier")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    m.get("product_name")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                )
            });
        out.push((
            pid,
            ServerInfo {
                port,
                token,
                identifier,
                product_name,
            },
        ));
    }
    out
}

/// The discovered backends as `ServerInfo` (dropping the pid) — for callers that only need
/// the server list, e.g. token lookup for a `VICTAURI_PORT` override.
fn discover_servers() -> Vec<ServerInfo> {
    discover_entries().into_iter().map(|(_, s)| s).collect()
}

/// Normalize a configured `VICTAURI_AUTH_TOKEN` value: an empty or whitespace-only
/// token is "not configured" (`None`), never an empty Bearer header.
///
/// The MCP server's contract is "auth is on by default unless `auth_disabled()`": the
/// plugin builder's `resolve_auth_token` generates a real token rather than disabling
/// auth when the configured token is blank. This is the client-side mirror — a botched
/// `VICTAURI_AUTH_TOKEN=""` must NOT make the bridge send an empty token to an
/// auth-enabled server (which would 401 every call); it must fall through to the token
/// discovered for the target port. Matches the filter used by every other token read
/// site (`discover_servers`, `victauri-test` discovery/app/client).
fn normalize_env_token(raw: Option<String>) -> Option<String> {
    raw.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Token belonging to the exact server selected by a `VICTAURI_PORT` override.
///
/// Never send a token discovered for one app to an unrelated localhost port.
fn discover_token_for_port(port: u16) -> Option<String> {
    token_for_port(&discover_servers(), port)
}

fn token_for_port(servers: &[ServerInfo], port: u16) -> Option<String> {
    servers
        .iter()
        .find(|server| server.port == port)
        .and_then(|server| server.token.clone())
}

/// Shared, warm HTTP client for `/health` probes. Built ONCE (rebuilding per call incurred
/// cold-start latency that made the FIRST probe against a live server intermittently exceed a
/// tight timeout — so discovery missed a running app on first contact). The CONNECT is bounded
/// tightly so a closed/filtered local port fast-fails (Windows does not always send a prompt
/// RST for a closed loopback port), while the total timeout stays generous: a live /health
/// answers in <50ms, so the larger budget only ever applies to the rare connected-but-
/// unresponsive reused port.
fn health_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(1200))
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

async fn health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    health_client()
        .get(&url)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Resolve a System32 executable from `%SystemRoot%` instead of `PATH`, so an attacker-writable
/// `PATH` entry cannot shadow it. The availability poller spawns `tasklist` repeatedly, so this
/// PATH-hijack surface is continuous — always invoke it by absolute path.
#[cfg(windows)]
fn system32_exe(name: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{root}\\System32\\{name}")
}

/// Resolve a core utility to an absolute path (defeats `PATH`-hijack of the poller's repeated
/// `ps`/`kill` spawns), falling back to the bare name only if neither canonical location exists.
#[cfg(not(windows))]
fn abs_bin(name: &str) -> String {
    for base in ["/bin", "/usr/bin"] {
        let p = format!("{base}/{name}");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

/// Snapshot the set of currently-live PIDs in ONE OS call, so discovery cost stays O(1)
/// process spawns regardless of how many (possibly stale) discovery directories exist.
/// Returns `None` if enumeration fails or is empty, in which case callers fall back to the
/// per-pid `is_process_alive` (never worse than the previous behavior).
#[cfg(windows)]
fn alive_pids() -> Option<HashSet<u32>> {
    // One `tasklist` in CSV form (~60ms) lists every process; column 2 is the PID.
    let out = std::process::Command::new(system32_exe("tasklist.exe"))
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let set: HashSet<u32> = text
        .lines()
        .filter_map(|line| {
            // Rows look like: "name","pid","session","sessname","mem"
            line.split("\",\"")
                .nth(1)
                .and_then(|f| f.trim_matches('"').trim().parse::<u32>().ok())
        })
        .collect();
    (!set.is_empty()).then_some(set)
}

/// One `ps` lists every PID on both Linux and macOS (portable; `/proc` is Linux-only).
#[cfg(not(windows))]
fn alive_pids() -> Option<HashSet<u32>> {
    let out = std::process::Command::new(abs_bin("ps"))
        .args(["-A", "-o", "pid="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let set: HashSet<u32> = text
        .split_whitespace()
        .filter_map(|t| t.parse::<u32>().ok())
        .collect();
    (!set.is_empty()).then_some(set)
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new(system32_exe("tasklist.exe"))
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            out.contains(&pid.to_string())
        })
}

#[cfg(not(windows))]
fn is_process_alive(pid: u32) -> bool {
    // Portable POSIX liveness check. `/proc` is Linux-only — on macOS it does not exist,
    // so the old `/proc/{pid}` test always returned false and the bridge filtered out every
    // discovery entry (it could find NO server on macOS). `kill -0` sends no signal but
    // succeeds iff the process exists and is signalable by us — and discovery entries are
    // our own user's processes. Works identically on macOS and Linux.
    std::process::Command::new(abs_bin("kill"))
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Trust a discovery directory only if it is a real directory (not a symlink), owned by the
/// current user, and not group/other-writable. Mirrors `victauri-test::discovery::dir_is_trusted`
/// — the bridge had no such check (audit #15 read-side residual), and it is the path Claude Code
/// connects through. No `unsafe` (this crate is `#![forbid(unsafe_code)]`): the effective uid is
/// read back from an exclusively-created probe file.
#[cfg(unix)]
fn dir_is_trusted(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_dir() {
        return false; // reject symlinks / non-dirs
    }
    let Some(euid) = current_euid() else {
        return false; // can't establish our uid -> don't trust
    };
    meta.uid() == euid && (meta.permissions().mode() & 0o022) == 0
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
        let probe = std::env::temp_dir().join(format!(
            ".victauri_bridge_uidprobe_{}_{}",
            std::process::id(),
            sequence
        ));
        if let Some(uid) = uid_from_exclusive_probe(&probe) {
            return Some(uid);
        }
    }
    None
}

/// Create a UID probe without following a pre-planted symlink in the shared temp dir.
#[cfg(unix)]
fn uid_from_exclusive_probe(probe: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(probe)
        .ok()?;
    let uid = file.metadata().ok().map(|m| m.uid());
    drop(file);
    let _ = std::fs::remove_file(probe);
    uid
}

/// On Windows the per-user temp dir is not world-writable, so the shared-temp planting
/// attack does not apply; trust the directory.
#[cfg(not(unix))]
fn dir_is_trusted(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cold-start handshake: the bridge must answer `initialize` itself so the MCP server
    //    is connected even with no app running (the fix for the 30s handshake timeout). ──

    #[test]
    fn local_initialize_echoes_id_and_protocol_and_advertises_list_changed() {
        let client = json!({
            "jsonrpc": "2.0", "id": 7, "method": "initialize",
            "params": { "protocolVersion": "2025-03-26", "capabilities": {} }
        });
        let resp = local_initialize_response(&client);
        assert_eq!(resp["id"], 7, "must echo the client's request id");
        assert_eq!(resp["jsonrpc"], "2.0");
        let result = &resp["result"];
        // Echo the client's requested version so it is guaranteed-acceptable.
        assert_eq!(result["protocolVersion"], "2025-03-26");
        // listChanged is the keystone: it lets the client refresh its tool list when the app
        // comes up, replacing the fallback with the live set — no reconnect.
        assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
        assert_eq!(result["capabilities"]["resources"]["listChanged"], true);
        // Must NOT advertise resources.subscribe — the plugin deliberately doesn't (no
        // server-initiated push exists, and this proxy has no channel to deliver it), so
        // advertising it here would mislead a client into subscribing and waiting forever.
        assert!(
            result["capabilities"]["resources"]
                .get("subscribe")
                .is_none(),
            "must not advertise a subscribe capability the server cannot honor"
        );
        assert_eq!(result["serverInfo"]["name"], "victauri-bridge");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn local_initialize_falls_back_to_default_protocol_when_absent() {
        let client = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = local_initialize_response(&client);
        assert_eq!(resp["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn fallback_tools_covers_the_full_surface_with_valid_schemas() {
        let tools = fallback_tools();
        // The baked manifest is the whole tool surface (extracted from the plugin's #[tool]s).
        assert_eq!(
            tools.len(),
            35,
            "expected the full 35-tool fallback surface"
        );
        for t in &tools {
            assert!(t["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            // A permissive-but-valid JSON Schema so the client accepts the tool while down.
            assert_eq!(t["inputSchema"]["type"], "object");
        }
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "eval_js",
            "invoke_command",
            "query_db",
            "introspect",
            "screenshot",
        ] {
            assert!(
                names.contains(&expected),
                "fallback must include {expected}"
            );
        }
    }

    #[test]
    fn fallback_tools_response_is_a_valid_tools_list_result() {
        let resp = fallback_tools_response(&json!(42));
        assert_eq!(resp["id"], 42);
        assert!(
            resp["result"]["tools"]
                .as_array()
                .is_some_and(|a| a.len() == 35)
        );
    }

    #[test]
    fn empty_list_responses_use_the_right_result_key() {
        let is_empty_arr = |v: &Value| v.as_array().is_some_and(std::vec::Vec::is_empty);
        assert!(is_empty_arr(
            &empty_list_response("resources/list", &json!(1))["result"]["resources"]
        ));
        assert!(is_empty_arr(
            &empty_list_response("resources/templates/list", &json!(1))["result"]["resourceTemplates"]
        ));
        assert!(is_empty_arr(
            &empty_list_response("prompts/list", &json!(1))["result"]["prompts"]
        ));
    }

    #[test]
    fn unreachable_message_is_actionable() {
        let m = unreachable_message();
        // Names the cause and the one-line fix so an agent doesn't fall back to CDP.
        assert!(m.contains("tauri dev"), "must name how to start the app");
        assert!(m.to_lowercase().contains("not reachable") || m.contains("no running"));
    }

    #[test]
    fn alive_pids_enumerates_and_includes_self() {
        // The batched liveness snapshot (one OS call) keeps discovery fast even with many
        // stale discovery dirs. On a normal host it succeeds and lists our own process; if the
        // platform enumerator is somehow unavailable it returns None and callers fall back.
        if let Some(set) = alive_pids() {
            assert!(
                set.contains(&std::process::id()),
                "the live-pid snapshot must include our own running process"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn uid_probe_refuses_preplanted_symlink_without_clobbering_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let probe = dir.path().join("probe");
        std::fs::write(&target, "must-survive").unwrap();
        std::os::unix::fs::symlink(&target, &probe).unwrap();

        assert_eq!(uid_from_exclusive_probe(&probe), None);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "must-survive");
    }

    fn srv(id: &str, name: &str, port: u16) -> ServerInfo {
        ServerInfo {
            port,
            token: None,
            identifier: Some(id.to_string()),
            product_name: Some(name.to_string()),
        }
    }

    #[test]
    fn selects_sole_server_without_app() {
        let live = vec![srv("com.a.app", "A", 7373)];
        assert!(matches!(select(&live, None), Selection::One(s) if s.port == 7373));
    }

    #[test]
    fn ambiguous_when_multiple_and_no_app() {
        let live = vec![srv("com.a.app", "A", 7373), srv("com.b.app", "B", 7374)];
        assert!(matches!(select(&live, None), Selection::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn selects_by_identifier_among_many() {
        let live = vec![srv("com.a.app", "A", 7373), srv("com.4da.app", "4DA", 7374)];
        match select(&live, Some("com.4da.app")) {
            Selection::One(s) => assert_eq!(s.port, 7374),
            _ => panic!("should pick 4DA by identifier"),
        }
    }

    #[test]
    fn selects_by_product_name_case_insensitive() {
        let live = vec![
            srv("com.a.app", "Demo", 7373),
            srv("com.4da.app", "4DA", 7374),
        ];
        match select(&live, Some("4da")) {
            Selection::One(s) => assert_eq!(s.port, 7374),
            _ => panic!("should pick by product name"),
        }
    }

    #[test]
    fn no_match_returns_none() {
        let live = vec![srv("com.a.app", "A", 7373)];
        assert!(matches!(
            select(&live, Some("com.nope.app")),
            Selection::None
        ));
    }

    #[test]
    fn token_selection_never_crosses_ports() {
        let mut first = srv("com.a.app", "A", 7373);
        first.token = Some("token-a".to_string());
        let mut second = srv("com.b.app", "B", 7374);
        second.token = Some("token-b".to_string());
        let servers = vec![first, second];

        assert_eq!(token_for_port(&servers, 7374).as_deref(), Some("token-b"));
        assert_eq!(token_for_port(&servers, 7999), None);
    }

    #[test]
    fn substring_identifier_match() {
        let live = vec![srv("com.victauri.demo", "Demo", 7373)];
        match select(&live, Some("demo")) {
            Selection::One(s) => assert_eq!(s.port, 7373),
            _ => panic!("substring of product/identifier should match"),
        }
    }

    // End-to-end against REAL discovery files: the plugin writes port/token/metadata.json
    // under `<temp>/victauri/<pid>/`; this proves the bridge parses those real files and can
    // select the right app by identity — even amid the many stale dirs left by dead processes.
    #[test]
    fn discover_servers_reads_real_metadata_and_selects() {
        let pid = std::process::id(); // alive → passes is_process_alive
        let dir = std::env::temp_dir().join("victauri").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // Make ownership/permissions deterministic so `dir_is_trusted` passes regardless of
        // the runner's umask (a umask of 002 would otherwise leave the dir group-writable
        // and the read-side trust guard would correctly reject it).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(dir.join("port"), "61999").unwrap();
        std::fs::write(dir.join("token"), "tok-xyz").unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            r#"{"pid":1,"port":61999,"identifier":"com.test.discover","product_name":"DiscoverTest"}"#,
        )
        .unwrap();

        let servers = discover_servers();
        let mine = servers
            .iter()
            .find(|s| s.identifier.as_deref() == Some("com.test.discover"))
            .expect("bridge should discover the entry written for the live current pid");
        assert_eq!(mine.port, 61999);
        assert_eq!(mine.token.as_deref(), Some("tok-xyz"));
        assert_eq!(mine.product_name.as_deref(), Some("DiscoverTest"));

        // And selection by identity picks it out.
        assert!(matches!(
            select(std::slice::from_ref(mine), Some("com.test.discover")),
            Selection::One(_)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Audit #15 read-side: a planted, world-writable discovery dir (the shape an attacker
    // creates in a shared /tmp) must NOT be trusted, so its token is never read/sent.
    #[cfg(unix)]
    #[test]
    fn dir_is_trusted_rejects_world_writable_and_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("vic_trust_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Owned, 0700 -> trusted.
        let good = base.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(dir_is_trusted(&good), "0700 owner dir must be trusted");

        // Group/other-writable -> rejected.
        let bad = base.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!dir_is_trusted(&bad), "world-writable dir must be rejected");

        // Symlink (even to a trusted target) -> rejected (no symlink following).
        let link = base.join("link");
        let _ = std::os::unix::fs::symlink(&good, &link);
        assert!(!dir_is_trusted(&link), "symlinked dir must be rejected");

        let _ = std::fs::remove_dir_all(&base);
    }

    // Round-4 audit, blocker #3 (CLI empty-token fallback): a blank `VICTAURI_AUTH_TOKEN`
    // must be treated as "unset" so the bridge falls through to the discovered token, NOT
    // sent as an empty Bearer to an auth-enabled server (which would 401 every call).
    #[test]
    fn normalize_env_token_treats_blank_as_unset() {
        assert_eq!(normalize_env_token(None), None, "unset -> None");
        assert_eq!(
            normalize_env_token(Some(String::new())),
            None,
            "empty -> None"
        );
        assert_eq!(
            normalize_env_token(Some("   ".to_string())),
            None,
            "spaces -> None"
        );
        assert_eq!(
            normalize_env_token(Some("\t\r\n ".to_string())),
            None,
            "whitespace -> None"
        );
        assert_eq!(
            normalize_env_token(Some("real-token".to_string())).as_deref(),
            Some("real-token"),
            "real token preserved"
        );
        assert_eq!(
            normalize_env_token(Some("  padded  ".to_string())).as_deref(),
            Some("padded"),
            "surrounding whitespace trimmed"
        );
    }
}
