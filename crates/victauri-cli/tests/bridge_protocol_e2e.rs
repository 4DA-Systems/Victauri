//! End-to-end tests of the real `victauri bridge` binary's JSON-RPC edge cases (round-5 audit):
//! selector normalization and message shapes the bridge must not mishandle.
//!
//! Each test runs its own mock backend, writes a discovery entry under a PRIVATE temp root
//! (the bridge's `TMP`/`TEMP`/`TMPDIR`/`XDG_RUNTIME_DIR` point there), so the tests never see —
//! or are seen by — any real Victauri app or bridge on the machine, and can run in parallel.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running bridge bound (through a real discovery entry) to a mock backend.
struct Harness {
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
    _child: ChildGuard,
    _root: tempfile::TempDir,
}

impl Harness {
    /// Serve `mcp_routes` (plus `/health`) on an ephemeral port, write a discovery entry for it
    /// with a unique identity, and spawn the bridge. `app_arg` = pass `--app <identity>`;
    /// `env` is applied after the Victauri selector variables are cleared.
    async fn start(mcp_routes: Router, app_arg: bool, env: &[(&str, &str)]) -> Self {
        let router = mcp_routes.route("/health", get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ident = format!("com.test.bridge-protocol.{unique}");
        let root = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let dir = root.path().join("victauri").join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [root.path().join("victauri"), dir.clone()] {
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        std::fs::write(dir.join("port"), port.to_string()).unwrap();
        std::fs::write(dir.join("token"), "proto-token").unwrap();
        std::fs::write(
            dir.join("metadata.json"),
            json!({"pid": pid, "port": port, "identifier": ident, "product_name": "Proto"})
                .to_string(),
        )
        .unwrap();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_victauri"));
        for var in ["TMP", "TEMP", "TMPDIR", "XDG_RUNTIME_DIR"] {
            cmd.env(var, root.path());
        }
        for var in ["VICTAURI_APP", "VICTAURI_PORT", "VICTAURI_AUTH_TOKEN"] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.arg("bridge");
        if app_arg {
            cmd.args(["--app", ident.as_str()]);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn victauri bridge");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let stderr = Arc::new(Mutex::new(Vec::<String>::new()));
        let capture = Arc::clone(&stderr);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines() {
                let Ok(line) = line else { break };
                capture.lock().unwrap().push(line);
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

        let mut h = Self {
            stdin,
            rx,
            stderr,
            _child: ChildGuard(child),
            _root: root,
        };
        h.send(&json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{}}));
        let init = h.recv_reply();
        assert_eq!(init["id"], "init", "{init}");
        h.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        h
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The next line that is a reply (an object with an `id`, or a batch array), skipping the
    /// poller's id-less `list_changed` notifications.
    fn recv_reply(&self) -> Value {
        loop {
            let line = match self.rx.recv_timeout(Duration::from_secs(20)) {
                Ok(l) => l,
                Err(e) => {
                    let s = self.stderr.lock().unwrap().join("\n");
                    panic!("bridge produced no reply in time: {e}; stderr:\n{s}");
                }
            };
            let v: Value = serde_json::from_str(&line).expect("bridge stdout is JSON");
            if v.is_array() || v.get("id").is_some() {
                return v;
            }
        }
    }
}

/// A stateless mock backend (like the real plugin's default): no session ids.
#[derive(Clone, Default)]
struct Backend {
    tool_calls: Arc<AtomicU64>,
    /// JSON arrays (batches) that reached the backend.
    batches: Arc<AtomicU64>,
    /// Client->server JSON-RPC responses (no `method`) that reached the backend.
    responses: Arc<AtomicU64>,
    /// Messages with neither a `method` nor a `result`/`error` that reached the backend.
    malformed: Arc<AtomicU64>,
}

async fn stateless_mcp(
    axum::extract::State(b): axum::extract::State<Backend>,
    body: String,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if v.is_array() {
        // What rmcp 3.1.2 does with a batch (verified live): reject it, never execute it.
        b.batches.fetch_add(1, Ordering::SeqCst);
        return axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let id = v.get("id").cloned();
    if v.get("method").is_none() {
        if v.get("result").is_some() || v.get("error").is_some() {
            // A response to a server-initiated request: accepted, no body (as rmcp does).
            b.responses.fetch_add(1, Ordering::SeqCst);
            return axum::http::StatusCode::ACCEPTED.into_response();
        }
        b.malformed.fetch_add(1, Ordering::SeqCst);
    }
    match v.get("method").and_then(Value::as_str) {
        Some("tools/call") => {
            b.tool_calls.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"jsonrpc":"2.0","id":id,
                "result":{"content":[{"type":"text","text":"ok"}]}}))
            .into_response()
        }
        Some(m) if m.starts_with("notifications/") => {
            axum::http::StatusCode::ACCEPTED.into_response()
        }
        _ => axum::Json(json!({"jsonrpc":"2.0","id":id,"result":{}})).into_response(),
    }
}

fn backend_routes(b: &Backend) -> Router {
    Router::new()
        .route("/mcp", axum::routing::post(stateless_mcp))
        .with_state(b.clone())
}

/// R5-BR2: `VICTAURI_APP=` (set but empty, e.g. from a templated `.mcp.json`) must mean "no
/// selector", as it does for victauri-test and the watchdog — not "match an app named ''",
/// which made every call "backend not reachable" with the one running app sitting right there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_victauri_app_env_is_treated_as_unset() {
    for blank in ["", "   "] {
        let backend = Backend::default();
        let mut h =
            Harness::start(backend_routes(&backend), false, &[("VICTAURI_APP", blank)]).await;
        h.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"get_plugin_info","arguments":{}}}));
        let r = h.recv_reply();
        assert_eq!(r["id"], 2, "{r}");
        assert!(
            r.get("result").is_some(),
            "VICTAURI_APP={blank:?} must not select nothing: {r}"
        );
        assert_eq!(backend.tool_calls.load(Ordering::SeqCst), 1);
    }
}

/// R5-BR1: a JSON-RPC batch (a JSON array) is answered LOCALLY — MCP removed batching in
/// 2025-06-18 and the real server rejects it — with a JSON-RPC batch response: one -32600 error
/// per request element, carrying that element's id (notifications get none), so every pending
/// id the client holds is resolved. It is never forwarded (so never re-sent after a failure),
/// and an all-notification batch gets no reply at all, per JSON-RPC 2.0 §6.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_is_rejected_locally_with_one_error_per_request() {
    let backend = Backend::default();
    let mut h = Harness::start(backend_routes(&backend), true, &[]).await;

    h.send(&json!([
        {"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"eval_js","arguments":{}}},
        {"jsonrpc":"2.0","method":"notifications/progress","params":{}},
        {"jsonrpc":"2.0","id":"eleven","method":"ping"},
        42
    ]));
    let reply = h.recv_reply();
    let errs = reply
        .as_array()
        .unwrap_or_else(|| panic!("batch reply must be an array: {reply}"));
    let ids: Vec<&Value> = errs.iter().map(|e| &e["id"]).collect();
    assert_eq!(ids, [&json!(10), &json!("eleven"), &Value::Null], "{reply}");
    for e in errs {
        assert_eq!(e["jsonrpc"], "2.0");
        assert_eq!(e["error"]["code"], -32600, "{e}");
        assert!(
            e["error"]["message"].as_str().unwrap().contains("batch"),
            "{e}"
        );
    }

    // An empty batch is itself an invalid request: one error object, id null.
    h.send(&json!([]));
    let empty = h.recv_reply();
    assert_eq!(empty["id"], Value::Null, "{empty}");
    assert_eq!(empty["error"]["code"], -32600, "{empty}");

    // All notifications → no reply. The next reply seen must be the ping's.
    h.send(&json!([{"jsonrpc":"2.0","method":"notifications/progress","params":{}}]));
    h.send(&json!({"jsonrpc":"2.0","id":12,"method":"ping"}));
    let ping = h.recv_reply();
    assert_eq!(
        ping["id"], 12,
        "an all-notification batch must get no reply: {ping}"
    );

    assert_eq!(
        backend.batches.load(Ordering::SeqCst),
        0,
        "batches are never forwarded"
    );
    assert_eq!(backend.tool_calls.load(Ordering::SeqCst), 0);
}

/// R5-BR3: a message with an `id` but no `method` is a client->server RESPONSE (to a
/// server-initiated request such as sampling/elicitation/roots, which a stateful backend can
/// send inside an SSE stream the bridge relays). It is forwarded, but JSON-RPC never replies to
/// a response — the bridge used to answer the client's own reply with an invented error. A
/// message with neither a method nor a result/error is an Invalid Request, answered locally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_response_is_forwarded_and_never_answered() {
    let backend = Backend::default();
    let mut h = Harness::start(backend_routes(&backend), true, &[]).await;

    h.send(&json!({"jsonrpc":"2.0","id":"srv-1","result":{"roots":[]}}));
    h.send(&json!({"jsonrpc":"2.0","id":"srv-2","error":{"code":-1,"message":"declined"}}));
    // The loop handles stdin in order, so the ping's reply is the next one — unless the
    // bridge answered either response.
    h.send(&json!({"jsonrpc":"2.0","id":12,"method":"ping"}));
    let next = h.recv_reply();
    assert_eq!(
        next["id"], 12,
        "a client response must never be answered: {next}"
    );
    assert_eq!(
        backend.responses.load(Ordering::SeqCst),
        2,
        "responses are still forwarded to the backend"
    );

    h.send(&json!({"jsonrpc":"2.0","id":13}));
    let invalid = h.recv_reply();
    assert_eq!(invalid["id"], 13, "{invalid}");
    assert_eq!(invalid["error"]["code"], -32600, "{invalid}");
    assert_eq!(backend.malformed.load(Ordering::SeqCst), 0, "not forwarded");
}
