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
}

async fn stateless_mcp(
    axum::extract::State(b): axum::extract::State<Backend>,
    body: String,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let id = v.get("id").cloned();
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
