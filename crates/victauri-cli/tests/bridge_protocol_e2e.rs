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

/// Knobs for [`Harness::start_with`].
#[derive(Default)]
struct Opts<'a> {
    /// Pass `--app <the discovery entry's identity>`.
    app_arg: bool,
    /// Extra environment, applied after the Victauri variables are cleared. A value of
    /// `"{port}"` is replaced with the mock backend's port.
    env: &'a [(&'a str, &'a str)],
    /// Extra bridge arguments.
    args: &'a [&'a str],
    /// The identity the backend's `/info` reports; `None` = the discovery entry's own.
    info_identity: Option<&'a str>,
}

impl Harness {
    /// Serve `mcp_routes` (plus `/health`) on an ephemeral port, write a discovery entry for it
    /// with a unique identity, and spawn the bridge. `app_arg` = pass `--app <identity>`;
    /// `env` is applied after the Victauri selector variables are cleared.
    async fn start(mcp_routes: Router, app_arg: bool, env: &[(&str, &str)]) -> Self {
        Self::start_with(
            mcp_routes,
            Opts {
                app_arg,
                env,
                ..Opts::default()
            },
        )
        .await
    }

    async fn start_with(mcp_routes: Router, opts: Opts<'_>) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ident = format!("com.test.bridge-protocol.{unique}");
        // `/info` reports the app's identity, like the real plugin (the bridge confirms it
        // before binding an `--app`, R5B-BR6).
        let info = json!({
            "app_identifier": opts.info_identity.unwrap_or(ident.as_str()),
            "app_product_name": "Proto",
        });
        let router = mcp_routes
            .route("/health", get(|| async { "ok" }))
            .route("/info", get(move || async move { axum::Json(info) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

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
        for (k, v) in opts.env {
            let v = if *v == "{port}" {
                port.to_string()
            } else {
                (*v).to_string()
            };
            cmd.env(k, v);
        }
        cmd.arg("bridge");
        if opts.app_arg {
            cmd.args(["--app", ident.as_str()]);
        }
        cmd.args(opts.args);
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
    /// `notifications/cancelled` that reached the backend.
    cancelled: Arc<AtomicU64>,
    /// `slow` tool calls that have FINISHED on the backend.
    slow_done: Arc<AtomicU64>,
}

/// How long the mock's `slow` tool takes.
const SLOW_CALL: Duration = Duration::from_secs(4);

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
        Some("tools/call") if v.pointer("/params/name") == Some(&json!("broken_sse")) => {
            b.tool_calls.fetch_add(1, Ordering::SeqCst);
            // The call is running (a progress event is streamed), then the connection dies
            // before the result: the command may well have executed.
            let progress = format!(
                "data: {}

",
                json!({"jsonrpc":"2.0","method":"notifications/progress",
                       "params":{"progressToken":1,"progress":1}})
            );
            let body = futures_util::stream::unfold(0u8, move |step| {
                let progress = progress.clone();
                async move {
                    match step {
                        0 => Some((Ok::<_, std::io::Error>(progress), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            Some((Err(std::io::Error::other("app died mid-stream")), 2))
                        }
                        _ => None,
                    }
                }
            });
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from_stream(body))
                .unwrap()
        }
        Some("tools/call") if v.pointer("/params/name") == Some(&json!("slow")) => {
            b.tool_calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(SLOW_CALL).await;
            b.slow_done.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"jsonrpc":"2.0","id":id,
                "result":{"content":[{"type":"text","text":"slow done"}]}}))
            .into_response()
        }
        Some("tools/call") => {
            b.tool_calls.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({"jsonrpc":"2.0","id":id,
                "result":{"content":[{"type":"text","text":"ok"}]}}))
            .into_response()
        }
        Some(m) if m.starts_with("notifications/") => {
            if m == "notifications/cancelled" {
                b.cancelled.fetch_add(1, Ordering::SeqCst);
            }
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
    // Responses are forwarded by the ordered one-way task, and nothing is written for them, so
    // the ping's reply is the next one — unless the bridge answered either response.
    h.send(&json!({"jsonrpc":"2.0","id":12,"method":"ping"}));
    let next = h.recv_reply();
    assert_eq!(
        next["id"], 12,
        "a client response must never be answered: {next}"
    );
    assert!(
        wait_until(Duration::from_secs(10), || backend
            .responses
            .load(Ordering::SeqCst)
            == 2)
        .await,
        "responses are still forwarded to the backend"
    );

    h.send(&json!({"jsonrpc":"2.0","id":13}));
    let invalid = h.recv_reply();
    assert_eq!(invalid["id"], 13, "{invalid}");
    assert_eq!(invalid["error"]["code"], -32600, "{invalid}");
    assert_eq!(backend.malformed.load(Ordering::SeqCst), 0, "not forwarded");
}

/// R5-BR4: an SSE response that dies mid-stream (after a progress event) means the call was
/// delivered and may have run. The client must be told exactly that — and the call must not
/// be re-sent — instead of the misleading "empty or non-JSON response" (the body-read error
/// used to be swallowed by `unwrap_or_default`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_whose_sse_stream_dies_says_it_may_have_run() {
    let backend = Backend::default();
    let mut h = Harness::start(backend_routes(&backend), true, &[]).await;

    h.send(&json!({"jsonrpc":"2.0","id":20,"method":"tools/call",
        "params":{"name":"broken_sse","arguments":{}}}));
    let r = h.recv_reply();
    assert_eq!(r["id"], 20, "{r}");
    let msg = r["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("error reply: {r}"));
    assert!(
        msg.contains("NOT retried") && msg.contains("may already have taken effect"),
        "must say the call may have executed: {msg}"
    );
    assert!(!msg.contains("empty or non-JSON"), "{msg}");
    assert_eq!(
        backend.tool_calls.load(Ordering::SeqCst),
        1,
        "a possibly-executed tool call is never re-sent"
    );
}

/// Poll `cond` until it holds or `within` elapses.
async fn wait_until(within: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

/// The next reply, waiting on the async runtime's blocking pool (the mock backend shares the
/// runtime, so the test thread must not block it).
async fn next_reply(h: &Arc<Mutex<Harness>>) -> (Value, std::time::Instant) {
    let h = Arc::clone(h);
    tokio::task::spawn_blocking(move || {
        let r = h.lock().unwrap().recv_reply();
        (r, std::time::Instant::now())
    })
    .await
    .unwrap()
}

/// R5B-BR5: the stdio loop awaited each forward before reading the next line, so one long
/// `tools/call` (up to 330 s) stalled a `ping`, every parallel call, and the
/// `notifications/cancelled` meant to stop it. Requests now run concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_tool_call_blocks_neither_ping_nor_cancel_nor_parallel_calls() {
    let backend = Backend::default();
    let h = Harness::start(backend_routes(&backend), true, &[]).await;
    let h = Arc::new(Mutex::new(h));
    let sent = std::time::Instant::now();
    {
        let mut h = h.lock().unwrap();
        h.send(&json!({"jsonrpc":"2.0","id":30,"method":"tools/call",
            "params":{"name":"slow","arguments":{}}}));
        h.send(&json!({"jsonrpc":"2.0","id":31,"method":"tools/call",
            "params":{"name":"slow","arguments":{}}}));
        h.send(&json!({"jsonrpc":"2.0","id":32,"method":"ping"}));
        h.send(&json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":30,"reason":"user"}}));
    }

    // The ping is answered at once, ahead of both slow calls.
    let (ping, at) = next_reply(&h).await;
    assert_eq!(
        ping["id"], 32,
        "ping must not queue behind a slow call: {ping}"
    );
    assert!(at - sent < SLOW_CALL / 2, "ping took {:?}", at - sent);

    // The cancellation reaches the backend while the call it cancels is still running.
    assert!(
        wait_until(SLOW_CALL / 2, || backend.cancelled.load(Ordering::SeqCst)
            == 1)
        .await,
        "notifications/cancelled did not reach the backend promptly"
    );
    assert_eq!(
        backend.slow_done.load(Ordering::SeqCst),
        0,
        "the cancel must arrive while the call is in flight"
    );

    // Both slow calls ran in parallel: together they take ~one SLOW_CALL, not two.
    let (a, _) = next_reply(&h).await;
    let (b, done) = next_reply(&h).await;
    let mut ids = [a["id"].as_i64().unwrap(), b["id"].as_i64().unwrap()];
    ids.sort_unstable();
    assert_eq!(ids, [30, 31], "{a} {b}");
    assert!(
        done - sent < SLOW_CALL * 3 / 2,
        "parallel calls were serialized: {:?}",
        done - sent
    );
    assert_eq!(backend.tool_calls.load(Ordering::SeqCst), 2);
}

/// R5B-BR6: `--app` binds by discovery metadata + PID liveness. After a crash the app's stale
/// entry can carry a PID that was reused by another of our processes while a DIFFERENT app
/// (auth disabled) now holds the port — the bridge then silently drove the wrong app. It now
/// confirms the identity the server itself reports on `/info` before forwarding anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_whose_info_reports_another_identity_is_never_driven() {
    let backend = Backend::default();
    let mut h = Harness::start_with(
        backend_routes(&backend),
        Opts {
            app_arg: true,
            info_identity: Some("com.someone.else"),
            ..Opts::default()
        },
    )
    .await;
    h.send(&json!({"jsonrpc":"2.0","id":40,"method":"tools/call",
        "params":{"name":"invoke_command","arguments":{"command":"quit_app"}}}));
    let r = h.recv_reply();
    assert_eq!(r["id"], 40, "{r}");
    let msg = r["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("must refuse, got: {r}"));
    assert!(
        msg.contains("com.someone.else"),
        "names what it found: {msg}"
    );
    assert_eq!(
        backend.tool_calls.load(Ordering::SeqCst),
        0,
        "nothing may be sent to a server that is not the selected app"
    );
}
