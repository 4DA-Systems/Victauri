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
    /// Owning process, when known from discovery (`None` for a `VICTAURI_PORT` override).
    pid: Option<u32>,
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
        match self.pid {
            Some(pid) => format!("{name} (port {}, pid {pid})", self.port),
            None => format!("{name} (port {})", self.port),
        }
    }

    /// `--app` matches the bundle identifier or the product name EXACTLY (ASCII
    /// case-insensitive) — never a substring (R4-CLI2: `--app com.example` bound
    /// `com.example.victauri-demo`, contradicting "never a silent wrong-app binding").
    fn matches_app(&self, app: &str) -> bool {
        identity_matches(
            self.identifier.as_deref(),
            self.product_name.as_deref(),
            app,
        )
    }
}

/// An app selector matches a bundle identifier or a product name EXACTLY, ASCII
/// case-insensitive — the one rule for discovery metadata and for `/info` alike.
fn identity_matches(identifier: Option<&str>, product_name: Option<&str>, app: &str) -> bool {
    identifier.is_some_and(|i| i.eq_ignore_ascii_case(app))
        || product_name.is_some_and(|p| p.eq_ignore_ascii_case(app))
}

/// The effective app selector: `--app`, else `VICTAURI_APP`, each trimmed, and an empty or
/// whitespace-only value treated as NOT SET — exactly as victauri-test and the watchdog read
/// `VICTAURI_APP` (R5-BR2). A set-but-empty `VICTAURI_APP=` used to select an app named "",
/// which matches nothing, so every call reported "backend not reachable" with the app running.
fn resolve_app_selector(cli: Option<String>, env: Option<String>) -> Option<String> {
    let normalize = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    normalize(cli).or_else(|| normalize(env))
}

/// Most forwarded requests the bridge keeps in flight at once. Past this a request is refused
/// immediately with an error rather than queued without bound (the plugin caps concurrency
/// server-side too). Local methods (`ping`, `initialize`, …) are never subject to it.
const MAX_IN_FLIGHT: usize = 32;
/// Notifications and client responses waiting to be forwarded, in order. A flood beyond this
/// is dropped (with a note on stderr) instead of growing memory without bound.
const ONE_WAY_QUEUE: usize = 1024;
/// On stdin EOF, how long the bridge lets in-flight requests finish (and queued notifications
/// drain) before it exits.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// State shared by the stdin loop, the per-request tasks, the one-way forwarder and the
/// availability poller.
struct Bridge {
    http: reqwest::Client,
    /// `--app` / `VICTAURI_APP`, normalized.
    app: Option<String>,
    /// The backend MCP session (a stateful legacy backend only).
    session_id: Mutex<Option<String>>,
    /// The client's `initialize` message, cached so we can hand-shake the BACKEND (replay it)
    /// when we first forward a real request and after a restart. The client is answered
    /// locally, so it only ever sends `initialize` once.
    cached_init: Mutex<Option<Value>>,
    /// Set once a backend handshake returns no `Mcp-Session-Id`: the server is stateless, so
    /// there is no session to mint or lose and we must not re-`initialize` before every call.
    stateless: AtomicBool,
    /// Single-flight guard for the lazy backend handshake: concurrent requests that all find no
    /// session wait here for ONE handshake instead of racing several (R5B-BR5).
    handshake: tokio::sync::Mutex<()>,
    /// Last-known backend availability — SOLELY owned by the poller (see H1 in the audit): the
    /// request path never writes it, so the down→up edge is detected in exactly one place.
    backend_up: AtomicBool,
    /// Set once the client sends `notifications/initialized` (its OWN handshake completion), so
    /// the poller never emits a server notification before the client has finished initializing
    /// — not merely before we answered `initialize` (audit #3).
    client_ready: AtomicBool,
    /// stdout is shared by every writer; each write locks, emits complete line(s), and flushes,
    /// so replies and notifications never interleave mid-line.
    stdout: Mutex<std::io::Stdout>,
    /// Bounds the forwarded requests in flight ([`MAX_IN_FLIGHT`]).
    in_flight: Arc<tokio::sync::Semaphore>,
}

impl Bridge {
    fn new(http: reqwest::Client, app: Option<String>) -> Self {
        Self {
            http,
            app,
            session_id: Mutex::new(None),
            cached_init: Mutex::new(None),
            stateless: AtomicBool::new(false),
            handshake: tokio::sync::Mutex::new(()),
            backend_up: AtomicBool::new(false),
            client_ready: AtomicBool::new(false),
            stdout: Mutex::new(std::io::stdout()),
            in_flight: Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    /// Forget the backend session — only if it is still `used`: a concurrent request may already
    /// have minted a fresh one, which a stale reply to an OLD session must not wipe.
    fn clear_session_if(&self, used: Option<&str>) {
        let mut sid = locked(&self.session_id);
        if sid.as_deref() == used {
            *sid = None;
        }
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
/// Requests are handled CONCURRENTLY (R5B-BR5): local methods (`initialize`, `ping`, the
/// client's `notifications/initialized`, batches, invalid messages) are answered inline; every
/// forwarded request runs as its own task, so a long `tools/call` (up to 330 s) no longer
/// delays a `ping`, a parallel tool call, or a `notifications/cancelled` for that very call.
/// Notifications and client responses are forwarded promptly, in order, by one dedicated task.
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
    let app = resolve_app_selector(app, std::env::var("VICTAURI_APP").ok());
    let bridge = Arc::new(Bridge::new(build_client()?, app));

    spawn_availability_poller(Arc::clone(&bridge));

    // Notifications and client responses: forwarded in arrival order by ONE task, so a
    // `notifications/cancelled` reaches the backend while the call it cancels is still running,
    // and never waits behind it.
    let (one_way_tx, mut one_way_rx) = tokio::sync::mpsc::channel::<Value>(ONE_WAY_QUEUE);
    let forwarder = {
        let bridge = Arc::clone(&bridge);
        tokio::spawn(async move {
            while let Some(msg) = one_way_rx.recv().await {
                forward_one_way(&bridge, &msg).await;
            }
        })
    };

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
        handle_line(&bridge, &one_way_tx, line.trim());
    }

    // stdin closed: let what is already running finish (bounded), then exit.
    drop(one_way_tx);
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        let _ = forwarder.await;
        let _ = bridge.in_flight.acquire_many(MAX_IN_FLIGHT as u32).await;
    })
    .await;
    Ok(())
}

/// Handle one stdin line. Never awaits: everything that talks to a backend is handed to a task
/// (a request) or to the ordered one-way queue (a notification / client response).
fn handle_line(bridge: &Arc<Bridge>, one_way: &tokio::sync::mpsc::Sender<Value>, line: &str) {
    if line.is_empty() {
        return;
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("victauri-bridge: invalid JSON on stdin: {e}");
            return;
        }
    };

    // A JSON-RPC batch is answered here and NEVER forwarded (R5-BR1): see `batch_rejection`.
    if let Value::Array(items) = &msg {
        match batch_rejection(items) {
            Some(reply) => write_value(&bridge.stdout, &reply),
            None => eprintln!(
                "victauri-bridge: dropped a JSON-RPC batch of notifications/responses \
                 (batching is not supported)"
            ),
        }
        return;
    }

    // No `method`: a client->server RESPONSE, or an Invalid Request (R5-BR3).
    if msg.get("method").is_none() {
        if is_client_response(&msg) {
            // A reply to a server-initiated request (sampling/elicitation/roots), which a
            // stateful backend may send inside an SSE stream this bridge relays verbatim — so
            // it must reach the backend. JSON-RPC never answers a response.
            enqueue_one_way(one_way, msg);
        } else {
            write_value(
                &bridge.stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": msg.get("id").cloned().unwrap_or(Value::Null),
                    "error": {
                        "code": INVALID_REQUEST,
                        "message": "invalid request: a JSON-RPC request needs a `method`"
                    }
                }),
            );
        }
        return;
    }

    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    match method {
        // ── Answered locally, inline — never block on a backend ─────────────
        "initialize" => {
            // Cache for the lazy backend handshake, then reply immediately.
            *locked(&bridge.cached_init) = Some(msg.clone());
            write_value(&bridge.stdout, &local_initialize_response(&msg));
        }
        // The client's ack of OUR local initialize. It must never reach a backend (we
        // handshake the backend separately) and needs no response — but it is the point at
        // which the client is ready to receive server notifications, so mark it here (NOT on
        // `initialize`) so the poller can't emit `list_changed` before the client is ready.
        "notifications/initialized" => {
            bridge.client_ready.store(true, Ordering::Release);
            // If the backend already came up during the init-handshake window, the poller
            // saw that down→up edge but skipped the emit (the client wasn't ready yet), and
            // there is no fresh edge to emit on later. Emit once now so the client still
            // refreshes to the live tool list — idempotent and cheap.
            if bridge.backend_up.load(Ordering::Acquire) {
                write_notification(&bridge.stdout, "notifications/tools/list_changed");
                write_notification(&bridge.stdout, "notifications/resources/list_changed");
            }
        }
        "ping" => {
            write_value(
                &bridge.stdout,
                &json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            );
        }
        // Any other notification (incl. `notifications/cancelled`): forwarded, in order,
        // without waiting for in-flight requests.
        _ if !expects_reply(&msg) => enqueue_one_way(one_way, msg),
        // ── A forwarded request: its own task, bounded ──────────────────────
        _ => match Arc::clone(&bridge.in_flight).try_acquire_owned() {
            Ok(permit) => {
                let bridge = Arc::clone(bridge);
                tokio::spawn(async move {
                    let _permit = permit;
                    let payloads = handle_request(&bridge, &msg).await;
                    write_payloads(&bridge.stdout, &payloads);
                });
            }
            Err(_) => write_value(
                &bridge.stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32000,
                        "message": format!(
                            "victauri bridge: {MAX_IN_FLIGHT} requests are already in flight; \
                             retry when one completes"
                        )
                    }
                }),
            ),
        },
    }
}

/// Queue a notification / client response for the ordered one-way forwarder.
fn enqueue_one_way(one_way: &tokio::sync::mpsc::Sender<Value>, msg: Value) {
    if let Err(e) = one_way.try_send(msg) {
        let method = match &e {
            tokio::sync::mpsc::error::TrySendError::Full(m)
            | tokio::sync::mpsc::error::TrySendError::Closed(m) => m
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<response>")
                .to_string(),
        };
        eprintln!("victauri-bridge: dropped {method}: {ONE_WAY_QUEUE} messages already queued");
    }
}

/// Forward a notification or a client response. Nothing is ever written to the client: a
/// notification to a down backend is simply dropped, and JSON-RPC never answers a response.
async fn forward_one_way(bridge: &Bridge, msg: &Value) {
    let outcome = forward_with_retries(bridge, msg).await;
    if !is_client_response(msg) {
        return;
    }
    match outcome {
        ForwardResult::Accepted => {}
        ForwardResult::Payloads(p) if p.is_empty() => {}
        ForwardResult::Payloads(p) => eprintln!(
            "victauri-bridge: backend answered a client response (not relayed): {}",
            p.join(" ")
        ),
        ForwardResult::Unreachable(e) => {
            eprintln!("victauri-bridge: could not deliver a client response: {e}");
        }
    }
}

/// The reply line(s) for one forwarded request (a request always gets at least one).
async fn handle_request(bridge: &Bridge, msg: &Value) -> Vec<String> {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    match method {
        // ── List methods: live when up, graceful placeholder when down ───────
        "tools/list" => match forward_when_up(bridge, msg).await {
            Some(payloads) => payloads,
            // App down (or a transient fetch miss): serve the full baked fallback so the agent
            // still sees every tool. The live list arrives via `tools/list_changed` when the
            // app comes up.
            None => vec![fallback_tools_response(&id).to_string()],
        },
        "resources/list" | "resources/templates/list" | "prompts/list" => {
            match forward_when_up(bridge, msg).await {
                Some(payloads) => payloads,
                // Empty (but valid) result when down — avoids a startup error for a
                // capability we advertise; the real list arrives via `list_changed`.
                None => vec![empty_list_response(method, &id).to_string()],
            }
        }
        // ── Everything else (tools/call, resources/read, …) needs a live app ─
        _ => match forward_with_retries(bridge, msg).await {
            ForwardResult::Payloads(payloads) => payloads,
            // `forward_with_retries` synthesizes an error for a request answered with a bare
            // 202, so this is unreachable for a request; answer defensively anyway.
            ForwardResult::Accepted => vec![error_for_request(
                msg,
                -32603,
                "backend accepted the request with no response (HTTP 202)",
            )],
            ForwardResult::Unreachable(err_msg) => vec![error_for_request(msg, -32000, &err_msg)],
        },
    }
}

/// Whether `msg` is a JSON-RPC request the client is waiting on a reply to: it has both a
/// `method` and an `id`. Notifications (no `id`) and responses (no `method`) never get one.
fn expects_reply(msg: &Value) -> bool {
    msg.get("method").is_some() && msg.get("id").is_some()
}

/// A client->server JSON-RPC response: no `method`, and a `result` or an `error`.
fn is_client_response(msg: &Value) -> bool {
    msg.get("method").is_none() && (msg.get("result").is_some() || msg.get("error").is_some())
}

/// JSON-RPC "Invalid Request" — used for a batch, which MCP does not support.
const INVALID_REQUEST: i64 = -32600;
const BATCH_UNSUPPORTED: &str = "batch requests are not supported: MCP removed JSON-RPC \
     batching in protocol revision 2025-06-18; send each message as its own line";

/// The local reply to a JSON-RPC batch (a JSON array on stdin), or `None` when nothing may be
/// answered.
///
/// MCP removed batching in 2025-06-18 and the embedded server rejects a batch without executing
/// it, so the bridge never forwards one: forwarding only produced an id-less error the client
/// could not match (or, after a post-send failure, re-sent the whole batch). Instead it answers
/// the way JSON-RPC 2.0 §6 prescribes for a batch: an ARRAY holding one `-32600` error per
/// request element, each carrying that element's `id`, so every id the client is waiting on is
/// resolved. Notifications and responses inside the batch get no entry (JSON-RPC never answers
/// either); an element that is not an object gets an error with `id: null`. An empty array is
/// itself an invalid request → a single error object with `id: null`. A batch of only
/// notifications/responses → `None` (§6: "the Server MUST NOT return an empty Array").
fn batch_rejection(items: &[Value]) -> Option<Value> {
    let error = |id: Value| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": INVALID_REQUEST, "message": BATCH_UNSUPPORTED }
        })
    };
    if items.is_empty() {
        return Some(error(Value::Null));
    }
    let replies: Vec<Value> = items
        .iter()
        .filter_map(|item| {
            let Some(obj) = item.as_object() else {
                return Some(error(Value::Null));
            };
            let is_notification = obj.contains_key("method") && !obj.contains_key("id");
            if is_notification || is_client_response(item) {
                None
            } else {
                Some(error(obj.get("id").cloned().unwrap_or(Value::Null)))
            }
        })
        .collect();
    (!replies.is_empty()).then_some(Value::Array(replies))
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
fn write_value(stdout: &Mutex<std::io::Stdout>, v: &Value) {
    let mut o = locked(stdout);
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// Relay already-serialized JSON-RPC payload lines (from a backend response) verbatim.
fn write_payloads(stdout: &Mutex<std::io::Stdout>, payloads: &[String]) {
    let mut o = locked(stdout);
    for payload in payloads {
        let _ = writeln!(o, "{payload}");
    }
    let _ = o.flush();
}

/// Emit a server→client JSON-RPC notification (no id).
fn write_notification(stdout: &Mutex<std::io::Stdout>, method: &str) {
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

/// Whether a message whose forward failed with `err` may be safely re-sent.
///
/// Always true when the connection failed BEFORE the request was written (`is_connect`) — the
/// backend never saw it. Otherwise the request may already have been delivered and executed, so
/// only side-effect-free protocol methods are replayed; `tools/call` is not.
fn may_replay(msg: &Value, err: &anyhow::Error) -> bool {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if method != "tools/call" {
        return true;
    }
    err.downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_connect)
}

/// Whole-request HTTP timeout for a forwarded protocol message (lists, reads, …).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Whole-request HTTP timeout for a `tools/call` that sets no `timeout_ms`: the app's own
/// eval timeout (`VICTAURI_EVAL_TIMEOUT` / `eval_timeout`) can be up to 300 s, plus the
/// server's pre-wait headroom — a 120 s cut-off failed such calls at the bridge while the
/// app kept running them (R4-CLIT).
const TOOL_CALL_DEFAULT_TIMEOUT: Duration = Duration::from_secs(330);
/// Headroom on top of a tool call's own `timeout_ms`: before its wait starts the server may
/// wait for the bridge, probe it (up to 2s) and make main-thread round trips (up to 10s each).
const TOOL_TIMEOUT_HEADROOM: Duration = Duration::from_secs(40);
/// The server's ceiling for any per-call `timeout_ms` (`invoke_command`; `wait_for` caps lower).
const MAX_TOOL_TIMEOUT_MS: u64 = 300_000;

/// HTTP timeout for forwarding `msg`. A tool call that blocks server-side for a caller-chosen
/// `timeout_ms` (`invoke_command` up to 300s, `wait_for` up to 120s) must outlast that wait;
/// with the fixed 120s timeout such calls failed at the bridge while the command kept running.
fn request_timeout_for(msg: &Value) -> Duration {
    let is_tool_call = msg.get("method").and_then(Value::as_str) == Some("tools/call");
    let fallback = if is_tool_call {
        TOOL_CALL_DEFAULT_TIMEOUT
    } else {
        DEFAULT_REQUEST_TIMEOUT
    };
    msg.pointer("/params/arguments/timeout_ms")
        .and_then(Value::as_u64)
        .map_or(fallback, |ms| {
            DEFAULT_REQUEST_TIMEOUT
                .max(Duration::from_millis(ms.min(MAX_TOOL_TIMEOUT_MS)) + TOOL_TIMEOUT_HEADROOM)
        })
}

/// Error text for a tool call that was (probably) delivered but got no response.
fn undelivered_response_message(msg: &Value, err: &anyhow::Error, app_still_up: bool) -> String {
    let timed_out = err
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_timeout);
    let what = if timed_out {
        format!(
            "the tool call was sent to the app but no response arrived before the bridge's \
             {}s timeout",
            request_timeout_for(msg).as_secs()
        )
    } else if app_still_up {
        "the tool call was sent to the app but the connection closed before a response \
         arrived (the app is still running — it may have restarted or reloaded while handling \
         the call)"
            .to_string()
    } else {
        "the tool call was sent to the app, and the app exited before responding. This is the \
         expected outcome for a command that quits or restarts the app (e.g. a `quit_app` \
         command) — it most likely ran"
            .to_string()
    };
    format!(
        "{what}. It was NOT retried, because it may already have taken effect and replaying \
         it could run it twice. Check the app's state (or its log) before calling it again. \
         (transport error: {err})"
    )
}

/// Background task that watches for the backend becoming reachable and, on a down→up
/// transition, tells the client to refresh its tool/resource lists — so the baked fallback
/// is replaced by the live, version-accurate set with no `/mcp` reconnect.
fn spawn_availability_poller(bridge: Arc<Bridge>) {
    tokio::spawn(async move {
        loop {
            // Poll fast while DOWN (a freshly-started app is noticed within ~1.5s) and back off
            // while UP (we only need to catch a later restart).
            let interval = if bridge.backend_up.load(Ordering::Acquire) {
                POLL_INTERVAL_UP_MS
            } else {
                POLL_INTERVAL_MS
            };
            tokio::time::sleep(Duration::from_millis(interval)).await;
            let up = discover_one(bridge.app.as_deref()).await.is_some();
            if !up {
                // Backend gone — drop the session so nothing can reuse it (defense-in-depth for
                // audit #1; the request path also re-resolves the trusted entry on every call).
                *locked(&bridge.session_id) = None;
                bridge.stateless.store(false, Ordering::Release);
            }
            // The poller is the SOLE owner of `backend_up`: the request path never writes it,
            // so the down→up edge is detected here exactly once and can never be silently
            // consumed by a tool call that happened to reconnect first.
            let was = bridge.backend_up.swap(up, Ordering::AcqRel);
            // Only announce a refresh once the CLIENT has finished initializing (sent
            // `notifications/initialized`) — a server notification before that is a lifecycle
            // violation a strict client may reject.
            if up && !was && bridge.client_ready.load(Ordering::Acquire) {
                write_notification(&bridge.stdout, "notifications/tools/list_changed");
                write_notification(&bridge.stdout, "notifications/resources/list_changed");
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
async fn forward_when_up(bridge: &Bridge, msg: &Value) -> Option<Vec<String>> {
    match forward_with_retries(bridge, msg).await {
        ForwardResult::Payloads(payloads) => Some(payloads),
        _ => None,
    }
}

/// Establish the backend MCP session if we have none (first forward, or after a restart
/// invalidated it) — ONE handshake at a time: concurrent requests that all find no session
/// wait for the first one's instead of racing their own (R5B-BR5). This is what makes
/// restart-recovery work — replaying a tool call with no session would 422.
async fn ensure_backend_session(bridge: &Bridge, info: &ServerInfo) {
    let needed =
        || !bridge.stateless.load(Ordering::Acquire) && locked(&bridge.session_id).is_none();
    if !needed() {
        return;
    }
    let _single_flight = bridge.handshake.lock().await;
    if !needed() {
        return; // another request completed the handshake while we waited
    }
    let init = locked(&bridge.cached_init).clone();
    let Some(init) = init else {
        return;
    };
    let token = info.token.as_deref();
    // We don't relay the backend handshake response to the client; it already believes it
    // is initialized (we answered locally).
    let Ok(out) = post_message(&bridge.http, info.port, token, None, &init).await else {
        return;
    };
    let backend_sid = out.session_id.clone();
    if let Some(sid) = out.session_id {
        *locked(&bridge.session_id) = Some(sid);
    } else if !out.stale_session {
        // Handshake succeeded with no session id → stateless backend.
        bridge.stateless.store(true, Ordering::Release);
    }
    // Complete the MCP lifecycle for a STATEFUL backend (harmless for the stateless default):
    // the client's `notifications/initialized` was answered locally, so replay it to the
    // backend now — a stateful rmcp session may gate tool calls on having received it.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let _ = post_message(
        &bridge.http,
        info.port,
        token,
        backend_sid.as_deref(),
        &note,
    )
    .await;
}

/// Forward a message to the live backend, establishing/recovering the backend session and
/// retrying across a restart. Discovers a backend first; returns `Unreachable` (never blocks
/// indefinitely) when no app is running.
async fn forward_with_retries(bridge: &Bridge, msg: &Value) -> ForwardResult {
    let app = bridge.app.as_deref();
    // Only a request (method + id) is owed a reply; never synthesize one for a notification or
    // for a client response (R5-BR3).
    let is_notification = !expects_reply(msg);

    // SECURITY (audit #1): re-resolve the trusted backend on EVERY forward — never reuse a cached
    // `(port, token)` without re-confirming, right now, that the port still belongs to a live,
    // trusted, identity-matched app. `scan_once` re-applies `dir_is_trusted` + liveness + `--app`
    // identity and yields the port and token together. Without this, after the app shut down (its
    // discovery entry gone) an attacker who bound the freed port would receive the cached Bearer
    // token and could relay forged tool results. This is ONE discovery pass (no 1s-sleep retries):
    // a live app resolves fast; a down app fails fast to the actionable message / fallback. The
    // backend is held in a LOCAL for this forward, so concurrent forwards never see each other's.
    let mut info = match scan_once(app).await {
        Selection::One(info) => info,
        Selection::Ambiguous(labels) => {
            return ForwardResult::Unreachable(ambiguous_message(&labels));
        }
        // Fail-fast by design: a tool call issued in the sub-second window where a restarting
        // app has removed its old discovery entry but not yet written the new one gets the
        // actionable "unreachable" message rather than waiting. This is the price of the audit
        // #1 rule (never reuse a cached connection); the retry loop's restart patience still
        // applies once a live trusted backend is found, and the next call ~1s later succeeds.
        Selection::None => return ForwardResult::Unreachable(unreachable_message()),
        Selection::Refused(why) => return ForwardResult::Unreachable(why),
    };

    for attempt in 0..MAX_RETRIES {
        ensure_backend_session(bridge, &info).await;
        let sid = locked(&bridge.session_id).clone();

        match post_message(
            &bridge.http,
            info.port,
            info.token.as_deref(),
            sid.as_deref(),
            msg,
        )
        .await
        {
            Ok(out) => {
                if out.stale_session {
                    eprintln!(
                        "victauri-bridge: stale session (HTTP {}), re-establishing (attempt {}/{})",
                        out.status,
                        attempt + 1,
                        MAX_RETRIES
                    );
                    bridge.clear_session_if(sid.as_deref());
                    if attempt + 1 < MAX_RETRIES {
                        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                        if let Ok(new_info) = discover_and_select(false, app).await {
                            info = new_info;
                        }
                    }
                    continue;
                }
                if let Some(new_sid) = out.session_id {
                    *locked(&bridge.session_id) = Some(new_sid);
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
                bridge.clear_session_if(sid.as_deref());
                // A tool call that may already have REACHED the app must not be replayed: it
                // can have side effects (invoke_command, input, interact…), and the commonest
                // way to get here is a call that itself quit or restarted the app (`quit_app`)
                // — replaying it after a relaunch would run it twice, and reporting "backend not
                // reachable" (the old behavior) told the agent the call never happened.
                if !may_replay(msg, &e) {
                    let still_up = matches!(scan_once(app).await, Selection::One(_));
                    return ForwardResult::Payloads(vec![error_for_request(
                        msg,
                        -32000,
                        &undelivered_response_message(msg, &e, still_up),
                    )]);
                }
                if attempt + 1 < MAX_RETRIES {
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_DELAY_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    // Re-discover; the app may have restarted on a new port, or gone away.
                    match discover_and_select(false, app).await {
                        Ok(new_info) => {
                            eprintln!("victauri-bridge: reconnected to {}", new_info.label());
                            info = new_info;
                        }
                        Err(_) => return ForwardResult::Unreachable(unreachable_message()),
                    }
                }
            }
        }
    }

    // Retries exhausted — the app is down or perpetually restarting. Each failure was logged to
    // stderr already; the client gets the actionable remedy.
    ForwardResult::Unreachable(unreachable_message())
}

fn build_client() -> Result<reqwest::Client> {
    // The default; `post_message` sets each request's own (see `request_timeout_for`).
    reqwest::Client::builder()
        .timeout(DEFAULT_REQUEST_TIMEOUT)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(Into::into)
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
        .timeout(request_timeout_for(msg))
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
        // A body that fails mid-read (the app died or the connection dropped mid-SSE-stream,
        // after the request was delivered — possibly after progress events) is a POST-SEND
        // failure, so it propagates: the caller then never re-sends a tool call and tells the
        // client it may already have run. Swallowing it (`unwrap_or_default`) reported a
        // misleading "empty or non-JSON response" instead (R5-BR4).
        let body = resp.text().await?;

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
            // Relay a non-SSE body ONLY if it is valid JSON, and relay its COMPACT single-line
            // form. Never write non-JSON onto the client's JSON-RPC stream, and never split one
            // message across stdout lines: a valid but pretty-printed body would otherwise be
            // broken into several frames by `writeln!`. Re-serializing the parsed value guarantees
            // exactly one line, matching the SSE path's per-line rigor. An empty or non-JSON body
            // yields no payload; the caller then synthesizes an error for a request (no hang).
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
                payloads.push(parsed.to_string());
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
        && let Ok(port) = p.trim().parse::<u16>()
        && health_ok(port).await
    {
        return port_override(port, app).await;
    }

    // Liveness-FIRST, then health. On Unix `alive_pids` snapshots our user's live PIDs in ONE
    // `ps` call, so many stale discovery dirs cost one spawn, not one each; on Windows each PID
    // is checked in-process (no spawn at all). Crucially we then health-check ONLY the
    // live-pid entries: a stale dir whose long-dead port was reused by some other service is
    // never probed (that mis-order made a down-state scan hang on an unresponsive reused port).
    // When the app is down there are zero live entries, so zero health probes — the poller
    // stays cheap. `None` (enumeration unavailable) falls back to the per-pid check.
    //
    // HONEST LIMITATION (documented residual, not closed here): liveness is PID-based, and PIDs
    // are reused. `/health` returns a static `ok` and carries no token, so it proves *something*
    // is bound but does not authenticate the listener as the real app. A stale `<pid>` dir whose
    // recorded PID has been recycled onto another of our live processes, combined with an attacker
    // binding the freed port, could still pass liveness+health and receive the token.
    // Re-resolving on every forward (audit #1) shrinks this to a per-call coincidence rather than
    // a cache-lifetime one. With `--app`, the identity the SERVER reports on `/info` must also
    // match (R5B-BR6) — which stops a different (e.g. auth-disabled) Victauri app that took the
    // port from being driven, but not a hostile listener that lies on `/info`; fully closing that
    // needs mutual auth (a plugin-side change).
    let entries = discover_entries();
    // Nothing discovered (the app is down): nothing to check, so no process enumeration —
    // the availability poller runs this every 1.5 s.
    if entries.is_empty() {
        return Selection::None;
    }
    let alive = alive_pids();
    let is_alive = |pid: u32| {
        alive
            .as_ref()
            .map_or_else(|| is_process_alive(pid), |set| set.contains(&pid))
    };
    let mut live = Vec::new();
    for (pid, s) in entries {
        if is_alive(pid) && health_ok(s.port).await {
            live.push(s);
        }
    }
    match (select(&live, app), app) {
        (Selection::One(info), Some(app)) => {
            match confirm_identity(&info, app, verified_backend()).await {
                Ok(()) => Selection::One(info),
                Err(why) => Selection::Refused(format!(
                    "No running Victauri app matches --app '{}': the discovery entry {} {why}.                      The entry is stale (the app exited and its PID was reused) or another app                      now holds its port. Start the app, or run `victauri doctor`.",
                    victauri_test::terminal::single_line(app),
                    victauri_test::terminal::single_line(&info.label()),
                )),
            }
        }
        (selection, _) => selection,
    }
}

/// A backend whose `/info` identity was confirmed for an app selector, so a later forward to
/// the SAME resolved entry (pid, port, token) needs no extra request.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedBackend {
    pid: Option<u32>,
    port: u16,
    token: Option<String>,
    app: String,
}

fn verified_backend() -> &'static Mutex<Option<VerifiedBackend>> {
    static VERIFIED: Mutex<Option<VerifiedBackend>> = Mutex::new(None);
    &VERIFIED
}

/// The identity the server on `port` reports on `/info` — `(app_identifier,
/// app_product_name)` — or `None` when it does not answer a well-formed `/info` (the token is
/// sent: `/info` is authenticated).
async fn fetch_identity(
    port: u16,
    token: Option<&str>,
) -> Option<(Option<String>, Option<String>)> {
    let mut req = health_client().get(format!("http://127.0.0.1:{port}/info"));
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let info: Value = resp.json().await.ok()?;
    let field = |k: &str| {
        info.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some((field("app_identifier"), field("app_product_name")))
}

/// Confirm that the server `info` resolved to really IS the app `app` selects, from what the
/// server itself reports on `/info` (R5B-BR6). Discovery metadata + PID liveness alone can
/// bind a crashed app's stale entry once its PID is reused, and with auth disabled nothing
/// else would notice that a DIFFERENT app now holds the port. A confirmation is cached for the
/// exact resolved entry (pid, port, token) — re-checked whenever the resolution changes; an
/// entry with no pid (a bare `VICTAURI_PORT`) is re-checked every time. Fails closed: a server
/// that does not report a matching identity is never used.
async fn confirm_identity(
    info: &ServerInfo,
    app: &str,
    cache: &Mutex<Option<VerifiedBackend>>,
) -> Result<(), String> {
    let key = VerifiedBackend {
        pid: info.pid,
        port: info.port,
        token: info.token.clone(),
        app: app.to_ascii_lowercase(),
    };
    if key.pid.is_some() && locked(cache).as_ref() == Some(&key) {
        return Ok(());
    }
    let reported = fetch_identity(info.port, info.token.as_deref()).await;
    let outcome = match &reported {
        Some((id, name)) if identity_matches(id.as_deref(), name.as_deref(), app) => Ok(()),
        Some((id, name)) => Err(format!(
            "points at a server that reports itself as '{}'",
            victauri_test::terminal::single_line(
                id.as_deref()
                    .or(name.as_deref())
                    .unwrap_or("<no app identity>")
            )
        )),
        None => Err("points at a server that did not confirm its identity on /info".to_string()),
    };
    *locked(cache) = match (&outcome, key.pid) {
        (Ok(()), Some(_)) => Some(key),
        _ => None,
    };
    outcome
}

/// The backend `VICTAURI_PORT` names. When an app selector (`--app` / `VICTAURI_APP`) is set
/// too, the two must agree (R5B-PORTAPP1) — the selector used to be silently ignored, so the
/// bridge drove whatever app held that port: first by the discovery metadata of the one live
/// entry on that port (if any), then by the identity the server itself reports on `/info`.
async fn port_override(port: u16, app: Option<&str>) -> Selection {
    let live = discover_live_servers();
    let entry = unique_entry_on_port(&live, port);
    let refuse = |why: &str| {
        Selection::Refused(format!(
            "VICTAURI_PORT={port} {why}, but the app selector (--app / VICTAURI_APP) is '{}'.              Unset one of them, or point VICTAURI_PORT at that app's port.",
            victauri_test::terminal::single_line(app.unwrap_or_default())
        ))
    };
    if let (Some(app), Some(e)) = (app, entry)
        && (e.identifier.is_some() || e.product_name.is_some())
        && !e.matches_app(app)
    {
        return refuse(&format!(
            "is app {}",
            victauri_test::terminal::single_line(&e.label())
        ));
    }
    let info = ServerInfo {
        pid: entry.and_then(|e| e.pid),
        port,
        // An EMPTY/whitespace `VICTAURI_AUTH_TOKEN` is "not configured", NOT "send an empty
        // Bearer" — it must fall through to the discovered token for this exact port (see
        // `normalize_env_token`).
        token: normalize_env_token(std::env::var("VICTAURI_AUTH_TOKEN").ok())
            .or_else(|| entry.and_then(|e| e.token.clone())),
        identifier: entry.and_then(|e| e.identifier.clone()),
        product_name: entry.and_then(|e| e.product_name.clone()),
    };
    if let Some(app) = app
        && let Err(why) = confirm_identity(&info, app, verified_backend()).await
    {
        return refuse(&why);
    }
    Selection::One(info)
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
                bail!("{}", ambiguous_message(&labels));
            }
            Selection::Refused(why) => bail!("{why}"),
        }
    }

    bail!("Could not connect to a matching Victauri server")
}

/// Several live apps match: name each (`identifier (port N, pid P)`) and how to pick one.
fn ambiguous_message(labels: &[String]) -> String {
    format!(
        "Multiple Victauri apps are running:\n  {}\nSelect one with `--app <bundle-identifier>` \
         or the VICTAURI_APP env var (an exact bundle identifier or product name). If two apps \
         share that identifier, stop one or pin the port with VICTAURI_PORT.",
        labels.join("\n  ")
    )
}

enum Selection {
    One(ServerInfo),
    None,
    Ambiguous(Vec<String>),
    /// A backend was resolved but must not be used: its identity does not match the app
    /// selector (R5B-BR6 / R5B-PORTAPP1). The message says why.
    Refused(String),
}

/// Pick the server matching `app` exactly, or the sole running server. Several matches —
/// no selector with several apps up, or two apps sharing an identifier — are ambiguous,
/// never "the first one".
fn select(live: &[ServerInfo], app: Option<&str>) -> Selection {
    let matching: Vec<&ServerInfo> = live
        .iter()
        .filter(|s| app.is_none_or(|app| s.matches_app(app)))
        .collect();
    match matching.as_slice() {
        [] => Selection::None,
        [one] => Selection::One((*one).clone()),
        many => Selection::Ambiguous(many.iter().map(|s| s.label()).collect()),
    }
}

/// Read `<temp>/victauri/<pid>/` discovery entries (port + token + identity) as
/// `(pid, ServerInfo)`. Pure filesystem work — NO process-liveness or HTTP health check here,
/// so it stays cheap even with many stale directories. Callers apply the health-then-liveness
/// filter in [`scan_once`], which is what keeps a down-state scan fast: a dead/stale port
/// refuses the connection instantly, so no process enumeration runs at all.
fn discover_entries() -> Vec<(u32, ServerInfo)> {
    let mut out = Vec::new();
    for root in discovery_roots() {
        discover_entries_in(&root, &mut out);
    }
    out
}

/// The discovery roots the plugin may have written to, most specific first (mirrors the
/// plugin's `discovery_root`). On Unix the root is per-user — `$XDG_RUNTIME_DIR/victauri` when
/// that directory is private to us, else `<temp>/victauri-<euid>` — and the legacy shared
/// `<temp>/victauri` is still read (a pre-0.9 plugin writes there) when it passes the same
/// ownership check. Other platforms use `<temp>/victauri` (a per-user temp dir).
#[cfg(not(test))]
fn discovery_roots() -> Vec<std::path::PathBuf> {
    real_discovery_roots()
}

/// Unit tests never read the machine's REAL discovery directories (a developer's running
/// apps and their tokens live there): every discovery path resolves against one private,
/// process-wide root instead.
#[cfg(test)]
fn discovery_roots() -> Vec<std::path::PathBuf> {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    vec![
        ROOT.get_or_init(|| tempfile::tempdir().expect("isolated discovery root"))
            .path()
            .to_path_buf(),
    ]
}

#[cfg_attr(all(test, not(unix)), allow(dead_code))]
fn real_discovery_roots() -> Vec<std::path::PathBuf> {
    let legacy = std::env::temp_dir().join("victauri");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut roots = Vec::new();
        if let Some(euid) = current_euid() {
            if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .filter(|dir| {
                    std::fs::symlink_metadata(dir).is_ok_and(|m| {
                        m.file_type().is_dir()
                            && m.uid() == euid
                            && m.permissions().mode() & 0o077 == 0
                    })
                })
            {
                roots.push(runtime.join("victauri"));
            }
            roots.push(std::env::temp_dir().join(format!("victauri-{euid}")));
        }
        roots.push(legacy);
        roots
    }
    #[cfg(not(unix))]
    {
        vec![legacy]
    }
}

fn discover_entries_in(root: &std::path::Path, out: &mut Vec<(u32, ServerInfo)>) {
    // The root itself is security-sensitive: its owner can rename a trusted PID
    // directory after our child check and swap in attacker-controlled files.
    if !dir_is_trusted(root) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
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
        // A pid already found under a more specific root counts once (a stale legacy entry
        // can carry a reused live pid).
        if out.iter().any(|(seen, _)| *seen == pid) {
            continue;
        }
        out.push((
            pid,
            ServerInfo {
                pid: Some(pid),
                port,
                token,
                identifier,
                product_name,
            },
        ));
    }
}

/// The discovered backends whose owning process is alive and ours — for token lookup for a
/// `VICTAURI_PORT` override (no health check: the caller already probed that port).
fn discover_live_servers() -> Vec<ServerInfo> {
    let entries = discover_entries();
    if entries.is_empty() {
        return Vec::new();
    }
    let alive = alive_pids();
    entries
        .into_iter()
        .filter(|(pid, _)| {
            alive
                .as_ref()
                .map_or_else(|| is_process_alive(*pid), |set| set.contains(pid))
        })
        .map(|(_, s)| s)
        .collect()
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

/// Token belonging to the exact server selected by a `VICTAURI_PORT` override (see
/// [`unique_entry_on_port`]).
#[cfg(test)]
fn token_for_port(servers: &[ServerInfo], port: u16) -> Option<String> {
    unique_entry_on_port(servers, port)?.token.clone()
}

/// The ONE live discovery entry advertising `port` — `None` when there is none, or several.
///
/// Never send a token discovered for one app to an unrelated localhost port: only the token
/// of the ONE live, trusted discovery entry advertising that port is used (R4-DISC2). A
/// stale entry of a dead app that once held the port — or two entries claiming it — yields
/// no token (and no identity), rather than the first match's.
fn unique_entry_on_port(servers: &[ServerInfo], port: u16) -> Option<&ServerInfo> {
    let mut matching = servers.iter().filter(|server| server.port == port);
    let server = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(server)
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

/// Whether a Victauri server answers on `port`. A `429` counts: `/health` is unauthenticated
/// and rate-limited from the public bucket, so any local process can flood it, and a
/// rate-limited reply still proves the server is up. Requiring a 2xx let such a flood make
/// every tool call fail with "backend not reachable — start the app" (R4-NET1, reproduced:
/// 88k × 429 → 3/3 bridge tool calls refused). The token is NOT sent on `/health`.
async fn health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    health_client()
        .get(&url)
        .send()
        .await
        .is_ok_and(|r| victauri_test::health_status_means_alive(r.status().as_u16()))
}

/// No batched process enumeration on this platform: callers fall back to per-pid checks.
#[cfg(not(any(unix, windows)))]
fn alive_pids() -> Option<HashSet<u32>> {
    None
}

/// Where core utilities live: the FHS locations, then the NixOS and Guix system profiles
/// (which have no `/bin/ps`). Never `PATH`: a hijacked `PATH` would decide which PIDs look
/// alive — and so where the Bearer token is sent (R5B-BR7).
#[cfg(unix)]
const BIN_DIRS: &[&str] = &[
    "/bin",
    "/usr/bin",
    "/run/current-system/sw/bin",
    "/run/current-system/profile/bin",
];

/// Resolve a core utility to an absolute path in [`BIN_DIRS`]; `None` when it is not
/// installed there (the caller then uses the hardened per-PID check, never a bare name).
#[cfg(unix)]
fn abs_bin(name: &str) -> Option<String> {
    BIN_DIRS
        .iter()
        .map(|dir| format!("{dir}/{name}"))
        .find(|p| std::path::Path::new(p).is_file())
}

/// Windows: no batched enumeration — each discovered PID is checked in-process by
/// [`is_process_alive`] (microseconds, no spawn). The `tasklist` snapshot this replaced cost
/// ~0.5 s per poll on an idle machine and spiked past 10 s under load, which stalled the
/// bridge's own replies; its per-PID fallback also substring-matched (PID 12 "matched" 123)
/// and counted other users' processes.
#[cfg(windows)]
fn alive_pids() -> Option<HashSet<u32>> {
    None
}

/// One `ps` lists the PIDs on both Linux and macOS (portable; `/proc` is Linux-only) — only
/// OUR OWN user's processes. `ps -A` counted every user's, so on a shared machine a stale
/// discovery entry whose PID had been recycled by ANOTHER user's process looked alive, and a
/// port squatter could then receive the token and feed forged tool results to the agent. The
/// per-pid fallback (`kill -0`) was already own-user only.
#[cfg(unix)]
fn alive_pids() -> Option<HashSet<u32>> {
    let uid = current_euid()?.to_string();
    let out = std::process::Command::new(abs_bin("ps")?)
        .args(["-U", uid.as_str(), "-o", "pid="])
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

/// A live process owned by the current user — exact PID, own-user only (audit R2-7) — via
/// victauri-test's hardened check, the same one its discovery and (by copy) the watchdog use.
/// On Unix that is `kill -0` from a fixed set of locations incl. NixOS/Guix, then a shell's
/// builtin `kill`, then `/proc` — never a `PATH` lookup. The bridge's own old copy fell back to
/// a bare `kill` on `PATH`, and found no app at all where no `kill` binary exists (R5B-BR7).
fn is_process_alive(pid: u32) -> bool {
    victauri_test::process::is_own_live_process(pid)
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
    for _ in 0..16 {
        // Unpredictable name (R4-DISC2): a guessable `<pid>_<seq>` name in the shared temp
        // dir let another user pre-create every probe path and deny us our own uid.
        let probe = std::env::temp_dir().join(format!(
            ".victauri_bridge_uidprobe_{}",
            uuid::Uuid::new_v4().simple()
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

/// Windows: trust a discovery directory only if it is a real directory (a symlink or junction
/// is not `is_dir()` under `symlink_metadata`) OWNED by the current user — its owner SID is the
/// token user, the token's default owner, or `BUILTIN\Administrators` when this token is a
/// member, the plugin writer's own rule — via victauri-test's check (this crate forbids
/// `unsafe`). `%TEMP%` is normally per-user, but a shared one (`C:\msys64\tmp` for an app
/// launched from MSYS2, a redirected `C:\Windows\Temp`) let another user plant
/// `victauri\<live pid>\` entries pointing at a port they control; the bridge then sent them
/// the agent's calls and relayed forged results. That residual is closed (R5B-WINDISC1).
#[cfg(windows)]
fn dir_is_trusted(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
        && victauri_test::process::dir_owned_by_current_user(path)
}

#[cfg(not(any(unix, windows)))]
fn dir_is_trusted(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit N6: the per-user root is scanned first; the legacy shared root is last.
    #[cfg(unix)]
    #[test]
    fn discovery_roots_are_per_user_first() {
        let roots = real_discovery_roots();
        let euid = current_euid().unwrap();
        assert!(roots.contains(&std::env::temp_dir().join(format!("victauri-{euid}"))));
        assert_eq!(roots.last(), Some(&std::env::temp_dir().join("victauri")));
        assert_ne!(roots[0], std::env::temp_dir().join("victauri"));
    }

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

    // ── Replay safety: a tool call that may have reached the app is never re-sent ──

    fn call(method: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}})
    }

    #[tokio::test]
    async fn connect_failure_is_replayable_even_for_tool_calls() {
        // Port 9 (discard) on loopback is closed on every CI host → a pre-send connect error.
        let err = reqwest::Client::new()
            .post("http://127.0.0.1:9/mcp")
            .send()
            .await
            .expect_err("nothing listens on :9");
        assert!(err.is_connect());
        assert!(may_replay(&call("tools/call"), &anyhow::Error::new(err)));
    }

    #[test]
    fn post_send_failure_is_not_replayed_for_tool_calls_only() {
        let err = anyhow::anyhow!("connection closed before message completed");
        assert!(!may_replay(&call("tools/call"), &err));
        assert!(may_replay(&call("tools/list"), &err));
        assert!(may_replay(&call("resources/read"), &err));
    }

    #[test]
    fn undelivered_message_says_the_call_likely_ran_when_the_app_exited() {
        let err = anyhow::anyhow!("connection reset");
        let gone = undelivered_response_message(&call("tools/call"), &err, false);
        assert!(gone.contains("exited before responding"), "{gone}");
        assert!(gone.contains("NOT retried"));
        assert!(
            !gone.contains("not reachable"),
            "must not claim the app was never reached"
        );
        let up = undelivered_response_message(&call("tools/call"), &err, true);
        assert!(up.contains("still running"), "{up}");
    }

    #[test]
    fn liveness_is_exact_and_own_user_only() {
        assert!(is_process_alive(std::process::id()));
        // The old Windows check substring-matched tasklist output, and counted every
        // account's processes. PID 4 (System) is live but never ours.
        #[cfg(windows)]
        assert!(!is_process_alive(4));
    }

    #[test]
    fn a_tool_call_timeout_outlasts_the_servers_own_wait() {
        let tool = |args: Value| {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "invoke_command", "arguments": args}})
        };
        assert_eq!(
            request_timeout_for(&call("tools/list")),
            DEFAULT_REQUEST_TIMEOUT
        );
        // R4-CLIT: with no `timeout_ms`, a tool call is bounded only by the app's eval
        // timeout (up to 300 s) — the bridge must not cut it off at 120 s.
        assert_eq!(
            request_timeout_for(&tool(json!({}))),
            TOOL_CALL_DEFAULT_TIMEOUT
        );
        assert!(TOOL_CALL_DEFAULT_TIMEOUT >= Duration::from_secs(330));
        assert_eq!(
            request_timeout_for(&tool(json!({"timeout_ms": 5_000}))),
            DEFAULT_REQUEST_TIMEOUT
        );
        // invoke_command at its 300s ceiling, and wait_for at its 120s one, finish at the
        // server before the bridge gives up on them.
        assert_eq!(
            request_timeout_for(&tool(json!({"timeout_ms": 300_000}))),
            Duration::from_secs(340)
        );
        assert_eq!(
            request_timeout_for(&tool(json!({"timeout_ms": 120_000}))),
            Duration::from_secs(160)
        );
        // Values past the server's ceiling are clamped the way the server clamps them.
        assert_eq!(
            request_timeout_for(&tool(json!({"timeout_ms": u64::MAX}))),
            Duration::from_secs(340)
        );
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

    /// R5B-BR7: a utility missing from the canonical locations (NixOS/Guix/minimal images)
    /// must never be resolved through `PATH` — a bare name lets a hijacked `PATH` decide which
    /// PIDs look alive, i.e. where the Bearer token goes.
    #[cfg(unix)]
    #[test]
    fn helper_binaries_are_never_resolved_through_path() {
        assert_eq!(abs_bin("victauri-no-such-helper"), None);
        if let Some(sh) = abs_bin("sh") {
            assert!(std::path::Path::new(&sh).is_absolute(), "{sh}");
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
            pid: Some(u32::from(port)),
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

    /// R4-CLI2 — this test used to assert the opposite (`--app demo` bound
    /// `com.victauri.demo` by substring). That was the bug: `--app com.example` silently bound
    /// `com.example.victauri-demo`. An exact identifier or product name still matches.
    #[test]
    fn app_selector_is_exact_never_substring() {
        let live = vec![srv("com.victauri.demo", "Demo App", 7373)];
        assert!(matches!(select(&live, Some("demo")), Selection::None));
        assert!(matches!(
            select(&live, Some("com.victauri")),
            Selection::None
        ));
        let live = vec![srv("com.example.victauri-demo", "Demo", 7373)];
        assert!(matches!(
            select(&live, Some("com.example")),
            Selection::None
        ));
        assert!(matches!(
            select(&live, Some("com.example.victauri-demo")),
            Selection::One(s) if s.port == 7373
        ));
        assert!(matches!(select(&live, Some("demo")), Selection::One(_)));
    }

    #[test]
    fn duplicate_identifiers_are_ambiguous_with_pids_and_ports() {
        let live = vec![
            srv("com.dup.app", "Dup", 7373),
            srv("com.dup.app", "Dup", 7374),
        ];
        match select(&live, Some("com.dup.app")) {
            Selection::Ambiguous(labels) => {
                assert_eq!(labels.len(), 2);
                assert!(labels[0].contains("port 7373") && labels[0].contains("pid 7373"));
                assert!(labels[1].contains("port 7374"));
                let msg = ambiguous_message(&labels);
                assert!(
                    msg.contains("--app") && msg.contains("VICTAURI_PORT"),
                    "{msg}"
                );
            }
            _ => panic!("two apps sharing an identifier must be ambiguous, not first-wins"),
        }
    }

    #[test]
    fn port_override_token_needs_one_live_entry_on_that_port() {
        // R4-DISC2: a VICTAURI_PORT override used the token of ANY discovery entry that once
        // advertised that port (first match), e.g. a dead app's stale entry.
        let mut stale = srv("com.dead.app", "Dead", 7374);
        stale.token = Some("stale-token".to_string());
        let mut live = srv("com.live.app", "Live", 7374);
        live.token = Some("live-token".to_string());
        assert_eq!(token_for_port(&[stale, live.clone()], 7374), None);
        assert_eq!(token_for_port(&[live], 7374).as_deref(), Some("live-token"));
    }

    /// A mock `/info` reporting `identifier`, counting requests.
    async fn info_server(identifier: &'static str) -> (u16, Arc<std::sync::atomic::AtomicU32>) {
        let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let app = axum::Router::new().route(
            "/info",
            axum::routing::get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    axum::Json(json!({"app_identifier": identifier, "app_product_name": "Name"}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, hits)
    }

    /// R5B-BR6: identity is confirmed from `/info`, cached per resolved (pid, port, token), and
    /// re-checked when the resolution changes.
    #[tokio::test]
    async fn app_identity_is_confirmed_once_per_resolved_entry() {
        let (port, hits) = info_server("com.real.app").await;
        let cache = Mutex::new(None);
        let mut info = srv("com.real.app", "Real", port);
        info.pid = Some(4242);

        assert!(
            confirm_identity(&info, "COM.REAL.APP", &cache)
                .await
                .is_ok()
        );
        assert!(
            confirm_identity(&info, "com.real.app", &cache)
                .await
                .is_ok()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "cached for the same entry");
        // The product name selects too, like discovery's `--app`.
        assert!(confirm_identity(&info, "name", &cache).await.is_ok());

        // A new token (the app restarted) is a different entry: re-checked.
        info.token = Some("fresh".into());
        assert!(
            confirm_identity(&info, "com.real.app", &cache)
                .await
                .is_ok()
        );
        let after_restart = hits.load(Ordering::SeqCst);
        assert!(after_restart >= 2, "{after_restart}");

        // Another app's selector: refused, naming what the server reported; never cached.
        let err = confirm_identity(&info, "com.other.app", &cache)
            .await
            .unwrap_err();
        assert!(err.contains("com.real.app"), "{err}");
        assert_eq!(*cache.lock().unwrap(), None);

        // No pid (a bare VICTAURI_PORT): checked every time.
        info.pid = None;
        let before = hits.load(Ordering::SeqCst);
        assert!(
            confirm_identity(&info, "com.real.app", &cache)
                .await
                .is_ok()
        );
        assert!(
            confirm_identity(&info, "com.real.app", &cache)
                .await
                .is_ok()
        );
        assert_eq!(hits.load(Ordering::SeqCst), before + 2);
    }

    #[tokio::test]
    async fn a_server_without_a_usable_info_is_refused() {
        // Nothing listens on :9 → no identity → fail closed.
        let mut info = srv("com.real.app", "Real", 9);
        info.pid = Some(1);
        let cache = Mutex::new(None);
        let err = confirm_identity(&info, "com.real.app", &cache)
            .await
            .unwrap_err();
        assert!(err.contains("did not confirm"), "{err}");
    }

    #[tokio::test]
    async fn a_rate_limited_health_endpoint_is_alive() {
        // R4-NET1: an unauthenticated /health flood (→ 429) made every bridge tool call fail
        // with "backend not reachable — start the app".
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(
            health_ok(port).await,
            "a 429 from /health proves the server is alive"
        );

        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(!health_ok(port).await);
    }

    // End-to-end against real-format discovery files: the plugin writes
    // port/token/metadata.json under `<root>/<pid>/`; this proves the bridge parses those
    // files and can select the right app by identity. (In unit tests `discovery_roots()` is a
    // private temp root — never the machine's real one.)
    #[test]
    fn discover_servers_reads_real_metadata_and_selects() {
        let pid = std::process::id(); // alive → passes is_process_alive
        let dir = discovery_roots()[0].join(pid.to_string());
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

        let servers = discover_live_servers();
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

    /// R5B-WINDISC1: a `<pid>` directory (or a whole root) owned by another account — SYSTEM
    /// stands in, which needs an elevated run to set up — is never read.
    #[cfg(windows)]
    #[test]
    fn windows_discovery_ignores_directories_owned_by_another_account() {
        let give_to_system = |p: &std::path::Path| {
            std::process::Command::new("icacls")
                .arg(p)
                .args(["/setowner", "*S-1-5-18", "/q"])
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let root = tempfile::tempdir().unwrap();
        for pid in ["21", "22"] {
            let dir = root.path().join(pid);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("port"), "7373").unwrap();
            std::fs::write(dir.join("token"), format!("tok-{pid}")).unwrap();
        }
        if !give_to_system(&root.path().join("22")) {
            eprintln!("skipped: cannot change a directory's owner (not elevated)");
            return;
        }
        let mut out = Vec::new();
        discover_entries_in(root.path(), &mut out);
        let pids: Vec<u32> = out.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(pids, [21], "a planted entry's token must never be read");

        assert!(give_to_system(root.path()));
        let mut out = Vec::new();
        discover_entries_in(root.path(), &mut out);
        assert!(out.is_empty(), "nothing under a foreign root is trusted");
    }

    /// R5-BR3: only a request (method + id) is owed a reply.
    #[test]
    fn only_requests_expect_a_reply() {
        assert!(expects_reply(&json!({"id": 1, "method": "tools/call"})));
        assert!(!expects_reply(&json!({"method": "notifications/x"})));
        assert!(!expects_reply(&json!({"id": 1, "result": {}})));
        assert!(is_client_response(&json!({"id": 1, "result": {}})));
        assert!(is_client_response(&json!({"id": 1, "error": {"code": 1}})));
        assert!(!is_client_response(&json!({"id": 1})));
        assert!(!is_client_response(
            &json!({"id": 1, "method": "m", "result": {}})
        ));
    }

    /// R5-BR1: a batch is rejected locally, one error per request element, per JSON-RPC §6.
    #[test]
    fn batch_rejection_answers_each_request_and_nothing_else() {
        let reply = batch_rejection(&[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled"}),
            json!({"jsonrpc":"2.0","id":"srv-1","result":{}}),
            json!({"jsonrpc":"2.0","id":2}),
            json!("junk"),
        ])
        .unwrap();
        let ids: Vec<Value> = reply
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].clone())
            .collect();
        assert_eq!(ids, [json!(1), json!(2), Value::Null]);
        assert!(
            reply
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["error"]["code"] == INVALID_REQUEST)
        );
        let empty = batch_rejection(&[]).unwrap();
        assert!(empty.is_object() && empty["id"].is_null());
        assert_eq!(
            batch_rejection(&[json!({"jsonrpc":"2.0","method":"notifications/x"})]),
            None
        );
    }

    /// R5-BR2: blank selectors are "unset", matching victauri-test and the watchdog.
    #[test]
    fn app_selector_treats_blank_as_unset() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(resolve_app_selector(None, None), None);
        assert_eq!(resolve_app_selector(None, s("")), None);
        assert_eq!(resolve_app_selector(None, s(" \t")), None);
        assert_eq!(resolve_app_selector(s(""), s("com.env")), s("com.env"));
        assert_eq!(resolve_app_selector(None, s(" com.env ")), s("com.env"));
        assert_eq!(
            resolve_app_selector(s("com.cli"), s("com.env")),
            s("com.cli")
        );
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
