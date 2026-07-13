//! End-to-end test of the real `victauri bridge` binary.
//!
//! Spawns the compiled binary, points it at a mock MCP server via real discovery files,
//! and proves the three guarantees that stop agents from falling back to CDP:
//!   1. it selects the backend **by app identity** (`--app`) from the discovery dir,
//!   2. it forwards a normal MCP session (initialize → tool call), and
//!   3. it **transparently recovers from a server restart** — when the session goes stale
//!      it re-initializes and replays the call, with no error surfaced to the host.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

/// Both e2e tests fake discovery by writing to `<temp>/victauri/<own-pid>/` — the SAME
/// directory (one per process). `cargo test` runs tests in parallel threads of one process,
/// so they must be serialized or they stomp each other's port/metadata files. A tokio mutex
/// (not std) so the guard can be held across the tests' `.await` points without tripping
/// `clippy::await_holding_lock`.
static E2E_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct Mock {
    valid_session: Arc<std::sync::Mutex<Option<String>>>,
    /// When set, the next `tools/call` with the current session is rejected (422), as if the
    /// app had restarted and dropped the session — until a fresh `initialize` mints a new one.
    restarted: Arc<AtomicBool>,
    init_count: Arc<AtomicU64>,
    toolcall_ok: Arc<AtomicU64>,
    /// Counts `notifications/initialized` the bridge forwards to the BACKEND after its
    /// handshake — proves the MCP lifecycle is completed for a stateful backend (audit M2).
    initialized_count: Arc<AtomicU64>,
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn mcp(State(s): State<Mock>, headers: HeaderMap, body: String) -> Response {
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let method = v["method"].as_str().unwrap_or("");
    let id = v.get("id").cloned();
    match method {
        "initialize" => {
            let n = s.init_count.fetch_add(1, Ordering::SeqCst) + 1;
            let sid = format!("sess-{n}");
            *s.valid_session.lock().unwrap() = Some(sid.clone());
            s.restarted.store(false, Ordering::SeqCst);
            let mut resp = Json(json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"protocolVersion":"2025-03-26","capabilities":{},
                           "serverInfo":{"name":"mock","version":"0"}}
            }))
            .into_response();
            resp.headers_mut()
                .insert("mcp-session-id", sid.parse().unwrap());
            resp
        }
        "notifications/initialized" => {
            s.initialized_count.fetch_add(1, Ordering::SeqCst);
            StatusCode::ACCEPTED.into_response()
        }
        // A distinguishable LIVE tool list, so a test can prove the bridge served the real
        // backend list (not its baked fallback) once the app came up.
        "tools/list" => Json(json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"tools": [{"name": "live_marker", "inputSchema": {"type": "object"}}]}
        }))
        .into_response(),
        "tools/call" => {
            let sid = headers
                .get("mcp-session-id")
                .and_then(|h| h.to_str().ok())
                .map(String::from);
            let valid = s.valid_session.lock().unwrap().clone();
            let stale = s.restarted.load(Ordering::SeqCst) || sid.is_none() || sid != valid;
            if stale {
                return (StatusCode::UNPROCESSABLE_ENTITY, "expect initialize").into_response();
            }
            s.toolcall_ok.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"content": [{"type": "text", "text": "{\"ok\":true}"}]}
            }))
            .into_response()
        }
        _ => Json(json!({"jsonrpc":"2.0","id":id,"result":{}})).into_response(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_selects_by_identity_forwards_and_survives_restart() {
    let _serial = E2E_SERIAL.lock().await;
    let mock = Mock {
        valid_session: Arc::new(std::sync::Mutex::new(None)),
        restarted: Arc::new(AtomicBool::new(false)),
        init_count: Arc::new(AtomicU64::new(0)),
        toolcall_ok: Arc::new(AtomicU64::new(0)),
        initialized_count: Arc::new(AtomicU64::new(0)),
    };

    // Start the mock backend on an ephemeral port.
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/mcp", post(mcp))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Write a REAL discovery entry (current pid → passes is_process_alive) tagged with our
    // app identity, so the bridge resolves us by `--app` exactly like a live Tauri app.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ident = format!("com.test.e2e-bridge.{unique}");
    let pid = std::process::id();
    let dir = std::env::temp_dir().join("victauri").join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let _dir_guard = DirGuard(dir.clone());
    std::fs::write(dir.join("port"), port.to_string()).unwrap();
    std::fs::write(dir.join("token"), "e2e-token").unwrap();
    std::fs::write(
        dir.join("metadata.json"),
        json!({"pid": pid, "port": port, "identifier": ident, "product_name": "E2E"}).to_string(),
    )
    .unwrap();

    // Spawn the real bridge binary, pinned to our app by identity.
    let mut child = ChildGuard {
        child: Command::new(env!("CARGO_BIN_EXE_victauri"))
            .args(["bridge", "--wait", "--app", ident.as_str()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn victauri bridge"),
    };

    let mut stdin = child.child.stdin.take().unwrap();
    let stdout = child.child.stdout.take().unwrap();
    let stderr = child.child.stderr.take().unwrap();
    let stderr_lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let stderr_capture = Arc::clone(&stderr_lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            stderr_capture.lock().unwrap().push(line);
        }
    });

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let recv = |rx: &mpsc::Receiver<String>| -> Value {
        // Skip server-initiated notifications (the availability poller emits
        // `tools/list_changed` / `resources/list_changed` once it sees the backend), which
        // are id-less and interleave with responses — a real client demuxes the same way.
        loop {
            let line = match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(line) => line,
                Err(e) => {
                    let stderr = stderr_lines.lock().unwrap().join("\n");
                    panic!("bridge produced no response in time: {e}; stderr:\n{stderr}");
                }
            };
            let v: Value = serde_json::from_str(&line).expect("bridge stdout is JSON");
            if v.get("id").is_some() {
                return v;
            }
        }
    };
    let send = |stdin: &mut std::process::ChildStdin, v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };

    // 1. initialize → forwarded, response relayed.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let init = recv(&rx);
    assert_eq!(init["id"], 1, "init response: {init}");
    assert!(init.get("result").is_some(), "init had a result: {init}");

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    // 2. tool call → forwarded, succeeds.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"eval_js","arguments":{"code":"1+1"}}}),
    );
    let r2 = recv(&rx);
    assert_eq!(r2["id"], 2);
    assert!(r2.get("result").is_some(), "tool call #1 succeeded: {r2}");

    // 3. SIMULATE A RESTART: invalidate the session. The next tool call will 422; the bridge
    //    must re-initialize and replay transparently — no error to the host.
    mock.restarted.store(true, Ordering::SeqCst);
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"eval_js","arguments":{"code":"2+2"}}}),
    );
    let r3 = recv(&rx);
    assert_eq!(r3["id"], 3, "post-restart response: {r3}");
    assert!(
        r3.get("result").is_some() && r3.get("error").is_none(),
        "tool call AFTER restart must transparently recover (not error): {r3}"
    );

    // The recovery re-initialized at least once more, and both tool calls actually executed.
    // Retry timing is intentionally tolerant: the product contract is transparent recovery,
    // not an exact hidden-handshake count under all scheduler timings.
    let init_count = mock.init_count.load(Ordering::SeqCst);
    assert!(
        (2..=4).contains(&init_count),
        "expected recovery re-initialization count in 2..=4, got {init_count}"
    );
    assert_eq!(
        mock.toolcall_ok.load(Ordering::SeqCst),
        2,
        "both tool calls should have executed on the backend"
    );
    // M2: after each backend handshake the bridge replays notifications/initialized to the
    // backend (the client's was answered locally), completing the stateful MCP lifecycle.
    assert!(
        mock.initialized_count.load(Ordering::SeqCst) >= 1,
        "bridge must forward notifications/initialized to the backend after its handshake"
    );

    drop(stdin);
}

/// The cold-start guarantee: a bridge launched with NO app running must still complete the
/// MCP handshake instantly (never hang 30s), present its full tool list from the baked
/// fallback, and report tool calls as cleanly unreachable — then, when the app comes up,
/// auto-emit `tools/list_changed` and start serving the LIVE list and real tool calls with no
/// reconnect. This is the regression guard for "every fresh terminal failed to connect."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_cold_start_serves_handshake_then_goes_live_when_app_appears() {
    let _serial = E2E_SERIAL.lock().await;

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ident = format!("com.test.coldstart.{unique}");
    let pid = std::process::id();
    let dir = std::env::temp_dir().join("victauri").join(pid.to_string());
    // Ensure we truly start "app down" — no leftover discovery entry for this pid.
    let _ = std::fs::remove_dir_all(&dir);
    let _dir_guard = DirGuard(dir.clone());

    // Spawn the bridge with NO backend present, pinned to our (currently-absent) app.
    let mut child = ChildGuard {
        child: Command::new(env!("CARGO_BIN_EXE_victauri"))
            .args(["bridge", "--app", ident.as_str()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn victauri bridge"),
    };
    let mut stdin = child.child.stdin.take().unwrap();
    let stdout = child.child.stdout.take().unwrap();
    let stderr = child.child.stderr.take().unwrap();
    let stderr_lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let stderr_capture = Arc::clone(&stderr_lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            stderr_capture.lock().unwrap().push(line);
        }
    });
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // Read the next raw JSON line (response OR server notification).
    let next = |rx: &mpsc::Receiver<String>| -> Value {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => serde_json::from_str(&line).expect("bridge stdout is JSON"),
            Err(e) => {
                let stderr = stderr_lines.lock().unwrap().join("\n");
                panic!("bridge produced no line in time: {e}; stderr:\n{stderr}");
            }
        }
    };
    // Next response carrying the given id (skips interleaved notifications).
    let recv_id = |rx: &mpsc::Receiver<String>, want: i64| -> Value {
        loop {
            let v = next(rx);
            if v.get("id").and_then(Value::as_i64) == Some(want) {
                return v;
            }
        }
    };
    let send = |stdin: &mut std::process::ChildStdin, v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };

    // 1. initialize is answered LOCALLY and instantly — the whole point (no 30s hang).
    let t0 = std::time::Instant::now();
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":100,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
    );
    let init = recv_id(&rx, 100);
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "handshake must be near-instant with no app running, took {:?}",
        t0.elapsed()
    );
    assert!(
        init.get("result").is_some(),
        "local init has a result: {init}"
    );
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"], true,
        "must advertise listChanged so the client refreshes when the app comes up"
    );

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    // 2. tools/list while DOWN → the full baked fallback (agent still sees every tool).
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":101,"method":"tools/list"}),
    );
    let list_down = recv_id(&rx, 101);
    let tools = list_down["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert_eq!(
        tools.len(),
        35,
        "fallback exposes the full tool surface: {list_down}"
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"eval_js"), "fallback names real tools");
    assert!(
        !names.contains(&"live_marker"),
        "must be the fallback, not the live list yet"
    );

    // 3. a tool CALL while down → clean, actionable error, not a hang.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":102,"method":"tools/call","params":{"name":"eval_js","arguments":{"code":"1+1"}}}),
    );
    let call_down = recv_id(&rx, 102);
    assert!(
        call_down.get("error").is_some(),
        "down tool call errors: {call_down}"
    );
    assert!(call_down.get("result").is_none());
    let emsg = call_down["error"]["message"].as_str().unwrap_or("");
    assert!(
        emsg.contains("tauri dev"),
        "error names how to start the app: {emsg}"
    );

    // 4. NOW bring the backend UP: start the mock and write a matching discovery entry.
    let mock = Mock {
        valid_session: Arc::new(std::sync::Mutex::new(None)),
        restarted: Arc::new(AtomicBool::new(false)),
        init_count: Arc::new(AtomicU64::new(0)),
        toolcall_ok: Arc::new(AtomicU64::new(0)),
        initialized_count: Arc::new(AtomicU64::new(0)),
    };
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/mcp", post(mcp))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(dir.join("port"), port.to_string()).unwrap();
    std::fs::write(dir.join("token"), "cold-token").unwrap();
    std::fs::write(
        dir.join("metadata.json"),
        json!({"pid": pid, "port": port, "identifier": ident, "product_name": "ColdStart"})
            .to_string(),
    )
    .unwrap();

    // 5. The poller must notice and PUSH `tools/list_changed` with no client action.
    let mut saw_list_changed = false;
    for _ in 0..40 {
        let v = next(&rx);
        if v.get("id").is_none() && v["method"] == "notifications/tools/list_changed" {
            saw_list_changed = true;
            break;
        }
    }
    assert!(
        saw_list_changed,
        "bridge must auto-emit tools/list_changed when the app comes up (no reconnect)"
    );

    // 6. tools/list now returns the LIVE backend list, not the fallback.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":103,"method":"tools/list"}),
    );
    let list_up = recv_id(&rx, 103);
    let live_names: Vec<&str> = list_up["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        live_names.contains(&"live_marker"),
        "after app-up, tools/list must serve the LIVE list: {list_up}"
    );

    // 7. and a real tool call now succeeds — end to end, no reconnect ever issued.
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":104,"method":"tools/call","params":{"name":"eval_js","arguments":{"code":"1+1"}}}),
    );
    let call_up = recv_id(&rx, 104);
    assert!(
        call_up.get("result").is_some() && call_up.get("error").is_none(),
        "tool call after app-up succeeds transparently: {call_up}"
    );
    assert_eq!(mock.toolcall_ok.load(Ordering::SeqCst), 1);

    drop(stdin);
}
